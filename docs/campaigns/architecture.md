# Campaigns — Architecture

Developer reference for the measurement-campaign subsystem: the IP
catalogue that seeds campaign targets, the enrichment pipeline that
populates catalogue geography, and the campaign backend itself
(data model, scheduler, reuse, HTTP surface). Operator-facing
workflow lives in [`user-guide.md`](user-guide.md).

## IP catalogue

`ip_catalogue` is the authoritative record for every IP meshmon knows
about. Every agent registration and every operator paste lands here.

### Table shape

| Column | Type | Notes |
|---|---|---|
| `id` | `UUID` | `gen_random_uuid()` default. |
| `ip` | `INET` | Unique host address (never a wider CIDR). |
| `display_name` | `TEXT` | Operator-facing label. |
| `city`, `country_code`, `country_name` | `TEXT` / `CHAR(2)` / `TEXT` | Geography. |
| `latitude`, `longitude` | `DOUBLE PRECISION` | Decimal degrees. |
| `asn`, `network_operator` | `INTEGER` / `TEXT` | BGP identity. |
| `website`, `notes` | `TEXT` | Free-form operator metadata. |
| `enrichment_status` | `enrichment_status` enum | `pending`, `enriched`, `failed`. |
| `enriched_at` | `TIMESTAMPTZ` | Last successful run. |
| `operator_edited_fields` | `TEXT[]` | Lock set; see Overrides below. |
| `source` | `catalogue_source` enum | `operator` or `agent_registration`. |
| `created_at`, `created_by` | `TIMESTAMPTZ` / `TEXT` | Creation audit. |

Indexes cover `country_code`, `asn`, and `(latitude, longitude)` for
filter queries, plus a GIN full-text index (`to_tsvector('simple', …)`)
over `display_name`, `city`, `country_name`, and `network_operator` that
powers the free-text filter. Notes are deliberately excluded — they are
operator memo, not search surface.

### `agents_with_catalogue` view

`agents` owns runtime fields only (`id`, `display_name`, `tcp_probe_port`,
`udp_probe_port`, `agent_version`, `registered_at`, `last_seen_at`, `ip`,
`location` free text). Geographic coordinates, city, country, ASN, and
network operator live on `ip_catalogue`. The `agents_with_catalogue`
`LEFT JOIN` view exposes the combined shape used by the agents page and
the campaign source filter — agents without a catalogue row still appear,
with null catalogue columns.

### Overrides

`operator_edited_fields TEXT[]` is the only override mechanism. Every
field name stored in the array is the PascalCase `Field::as_str()`
rendering (`Latitude`, `NetworkOperator`, …). The lock rule is:

- **UI edits** append every touched field to the array (via the PATCH
  handler and the repo `patch` write).
- **Paste metadata** (`PasteRequest.metadata` on `POST /api/catalogue`)
  runs through the same lock-aware merge as PATCH: new rows always
  accept the supplied values and lock them; existing rows receive a
  field only when it is not already in `operator_edited_fields`.
  Paired fields (`Latitude`+`Longitude`, `CountryCode`+`CountryName`)
  apply atomically — skipping both halves when either is locked — and
  the response's `skipped_summary` aggregates what was refused.
- **Agent self-report** on `AgentApi.Register` appends `Latitude` and
  `Longitude` — the agent's config was set by someone who knows where it
  physically sits, and that value wins over any provider geo.
- **Enrichment providers** skip any field listed in the array. The merge
  layer (`enrichment::MergedFields::apply`) consults the lock set before
  writing each column.
- **Revert to auto** (PATCH `revert_to_auto: [field, …]`) NULLs the
  column *and* removes the field name from the array, so the next
  enrichment pass re-populates it.

The field-name encoding is case-sensitive — `Latitude` is locked,
`latitude` is not. A unit test pins the `as_str` / `FromStr` round-trip;
divergence fails loudly.

## Enrichment pipeline

Enrichment is a fixed chain of pluggable providers, composed at boot by
[`enrichment::providers::build_chain`][chain] from the `[enrichment]`
config section.

[chain]: ../../crates/service/src/enrichment/providers/mod.rs

### Provider chain

1. **`ipgeolocation`** — richest field coverage (city, country, lat/lon,
   ASN, network operator). Default first in the chain. Subject to the
   provider's free-tier quota.
2. **`rdap`** — free, credential-less registry lookup via
   `icann-rdap-client`. Resolves the appropriate RIR via IANA
   bootstrap; caches the bootstrap registry in memory for the process
   lifetime. Fills ASN, network operator, and country for any field
   that `ipgeolocation` did not already supply. Enabled by default.
3. **`maxmind-geolite2`** (feature `enrichment-maxmind`, off by default) —
   local mmdb lookups; offline fallback for city / ASN.
4. **`whois`** (feature `enrichment-whois`, off by default) — last-resort
   network-operator fallback.

Each provider advertises the set of `Field`s it may populate via
`EnrichmentProvider::supported()` and performs one async lookup per IP.
Providers are pure — they compute fields and never touch the database.

### First-writer-wins

The runner walks the chain in declared order and merges each result into
a `MergedFields` accumulator. A provider writes a field only when the
destination slot is empty *and* the field is not locked. Earlier
providers therefore win on conflicts — chain ordering is the only
precedence knob.

A single final write applies the merged result through
`repo::apply_enrichment_result`, which uses `COALESCE` so existing
values (locked or operator-written) are never clobbered even in the
face of concurrent enrichment + PATCH.

### Failure classification

`EnrichmentError` variants drive runner behaviour:

| Variant | Retryable | Runner reaction |
|---|---|---|
| `RateLimited { retry_after }` | yes | Log and move on; the row stays `pending` and the sweep re-picks it. |
| `Unauthorized` | no | Log; the provider is effectively dead for the process because subsequent calls will 401 too. |
| `NotFound` | no | Terminal for this provider; the runner falls through to the next one. |
| `Transient(String)` | yes | Log; the chain continues, and the sweep re-picks the row if every provider failed. |
| `Permanent(String)` | no | Log; the chain continues. |

If every provider errored and `MergedFields::any_populated()` stays
false, the repo writes terminal `enrichment_status = 'failed'`.
Otherwise the row flips to `enriched`.

### ipgeolocation terms-of-service gate

The config loader refuses to boot with `[enrichment.ipgeolocation]
enabled = true` unless `acknowledged_tos = true`. The operator's
explicit acknowledgement is the only way to activate the paid provider
— a missing or `false` flag aborts startup with a clear error.

## Enrichment runner

[`enrichment::runner::Runner`][runner] is a single long-lived task that
drains work and persists merged results.

[runner]: ../../crates/service/src/enrichment/runner.rs

- **Queue.** An `mpsc` channel fed by write-path handlers (paste,
  agent register, re-enrich, bulk re-enrich). The producer
  (`EnrichmentQueue::enqueue`) is non-blocking: `try_send` drops on
  full with a `warn!` so a paste storm can't exhaust memory.
- **Sweep.** A `tokio::time::interval` (30 s in production, shorter in
  tests) scans for `enrichment_status = 'pending' AND created_at <
  NOW() - INTERVAL '30 seconds'` rows and processes up to 128 per
  cycle. The sweep is the safety net for queue-full drops and
  restarted processes.
- **Ordering.** A `biased` `tokio::select!` gives the queue priority
  over the sweep tick so fresh work overtakes stale work.
- **Per-row cycle.** Load the row → `mark_enrichment_start` → walk the
  chain → `apply_enrichment_result` (which returns the terminal
  `enriched`/`failed` status) → publish a single
  `CatalogueEvent::EnrichmentProgress` on the broker.

The runner is idempotent: provider output is stable and persistence
goes through `COALESCE` + the lock check, so re-running on the same row
produces the same state.

## Catalogue event broker and SSE

Every mutating catalogue operation publishes one `CatalogueEvent` on a
process-wide `tokio::sync::broadcast` channel. Events are:

- `Created { id, ip }`
- `Updated { id }`
- `Deleted { id }`
- `EnrichmentProgress { id, status }`

Capacity is fixed at 512. Publishers fire-and-forget — `send` errors
are ignored so a subscriber-less broker is still valid.

`GET /api/catalogue/stream` translates a per-connection subscription
into an SSE response. Events are serialised as JSON frames. A 15-second
keep-alive comment prevents idle-timeout from intermediate proxies.

If the subscriber's receiver falls behind the 512-slot buffer, the
stream wrapper surfaces `BroadcastStreamRecvError::Lagged(n)`. The
handler translates that into a synthetic frame
`{"kind":"lag","missed":N}` so clients can detect the gap and refetch
state rather than drift silently.

## Frontend — SSE and cache

The `/catalogue` page opens one `EventSource` connection to
`/api/catalogue/stream` for its entire lifetime. A single hook at the
page level receives all events:

- `created` and `updated` — `setQueryData` on the per-entry cache key
  (`['catalogue','entry',id]`) with the fresh payload.
- `deleted` — removes the entry from the list query and the per-entry
  cache.
- `enrichment_progress` — merges the new `enrichment_status` into the
  per-entry cache so `StatusChip` and `PasteStaging` re-render live.
- `lag` — triggers a full list refetch so the page re-syncs with the
  server after a burst that outpaced the 512-slot broadcast buffer.

Components read per-entry data directly from the cache
(`useQuery({ queryKey: ['catalogue','entry',id], enabled: false })`);
they never open a second SSE connection.

## Catalogue list — keyset paging, sort, server-side filters

`GET /api/catalogue` is the list surface for the catalogue page and
every downstream consumer (campaign composer, history picker).

- **Keyset pagination.** Each response carries `entries`, `total`, and
  `next_cursor`. `next_cursor` is an opaque base64-encoded JSON object
  `{s, d, v, i}` (sort column, direction, last sort-value, last id) and
  is `null` on the last page. Page size defaults to 100 and is
  hard-clamped to 500. A malformed or type-mismatched cursor is
  silently dropped and the request restarts from the first page — the
  cursor is advisory, not authoritative.
- **Single-column sort.** `sort` picks the column (`ip`, `display_name`,
  `city`, `country_code`, `asn`, `network_operator`, `enrichment_status`,
  `website`, `location` for the derived `has_coords` boolean, or
  `created_at`); `sort_dir` is `asc` or `desc`. Default is
  `created_at DESC`. Two invariants hold for every sort:
  - `NULLS LAST` in both directions (nullable columns keep nulls after
    every populated row regardless of direction).
  - `id DESC` is always the cursor tiebreaker, so pages don't repeat or
    skip rows when the sort column has duplicate values.
- **All filters run server-side.** `country_code[]`, `asn[]`,
  `network[]`, `city` (CSV), `ip_prefix`, `name`, and `shapes` (JSON
  array of `Polygon` rings in `[lng, lat]` order) compose with AND; ASN
  and country multi-selects compose with OR within the field. The
  backend runs a SQL bbox pre-filter from the union of the shape rings
  and a Rust `geo`-crate point-in-polygon pass on the returned page. No
  filter is client-side any more, so `total` always reflects the true
  filter size the operator sees.
- **`total` is approximate under shape filters.** The count query
  pre-filters by the shape union's bounding box but cannot subtract the
  polygon miss without materialising the whole filtered set; rows that
  land inside the bbox but outside every polygon are counted in
  `total` while the page walk excludes them from `entries`. Clients
  that need the exact shape-filtered count sum `entries.length` across
  every page. `total` is exact for every shape-free filter
  combination.
- **Shape wire shape.** Clients serialise circles, rectangles, and
  freeform polygons to the same `Polygon[]` wire type via
  `shapesToPolygons(shapes)` in `frontend/src/lib/geo.ts` (circles
  discretise to a 64-step polygon; rectangles convert to 4-vertex
  polygons). Polygon rings are `[lng, lat]` pairs — GeoJSON
  convention.

## Catalogue map endpoint

`GET /api/catalogue/map` powers the map tab. The endpoint is
viewport-scoped and shape-blind by design: operators need to draw
polygons against the unfiltered fleet geography, and likewise the city
filter narrows the table without distorting the map.

- **Required params.** `bbox` is an array `[minLat, minLng, maxLat, maxLng]`; `zoom` is an integer. Missing or malformed values produce a 400.
- **Text filters flow through.** `country_code[]`, `asn[]`, `network[]`,
  `ip_prefix`, and `name` are honoured exactly as on the list endpoint.
- **Shape-blind and city-blind.** `shapes` and `city` are not accepted
  on the wire. The backend DTO omits them and the frontend does not
  send them. Operators narrow the *table* with shapes and cities; the
  *map* stays showing the catalogue's real geographic coverage.
- **Adaptive response.** The response is a discriminated union keyed
  on `kind`:
  - `{ "kind": "detail", "rows": [...], "total": N }` when the
    filtered viewport row count is at or below
    `MAP_DETAIL_THRESHOLD` (2000). The client renders one pin per row.
  - `{ "kind": "clusters", "buckets": [...], "total": N, "cell_size": D }`
    otherwise. Each bucket carries a sample catalogue id, a `lat` /
    `lng` centroid, a `count`, and its own `bbox` — the frontend
    passes that bbox straight into a scoped
    `useCatalogueListInfinite` when the operator opens the cluster
    dialog.
- **Cell size.** `cell_size_for_zoom(zoom)` bands zoom levels into six
  steps: `0-2 → 10°`, `3-5 → 5°`, `6-8 → 1°`, `9-11 → 0.25°`,
  `12-14 → 0.05°`, `15+ → 0.01°`. Zooms beyond 20 fall back to the
  finest band.

## Agent Register hook

`AgentApi::Register` calls
`catalogue::repo::ensure_from_agent(&mut *tx, ip, lat, lon)` inside
the same transaction as the `agents` upsert, so a catalogue-sync
failure rolls back the agent write too. SSE publish and enrichment
enqueue happen after `tx.commit()`:

- Missing catalogue row → `INSERT` with `source =
  'agent_registration'` and `operator_edited_fields =
  ARRAY['Latitude', 'Longitude']`.
- Existing catalogue row → `UPDATE` latitude + longitude and
  union-merge `Latitude` / `Longitude` into
  `operator_edited_fields`.

The agent's self-reported geo is therefore authoritative and the
enrichment chain will never overwrite it. Other catalogue fields
(`city`, `country_code`, ASN, network operator) remain open for
providers to populate.

## Catalogue HTTP surface

All under `/api/catalogue`, all session-authenticated.

- `POST /api/catalogue` — operator paste; parses tokens, bulk-inserts
  accepted IPs, enqueues each new id for enrichment.
- `GET /api/catalogue` — keyset-paginated filtered list. Query params:
  `limit` (default 100, max 500), `after` (opaque cursor), `sort`,
  `sort_dir`, `country_code[]`, `asn[]`, `network[]`, `city` (CSV),
  `ip_prefix`, `name`, `shapes`. Response carries `entries`, `total`,
  `next_cursor`. See the "Catalogue list" section above for the full
  contract.
- `GET /api/catalogue/map` — viewport-scoped adaptive response.
  Required `bbox` + `zoom`; accepts the list endpoint's text filters
  but not `shapes` or `city`. Returns either
  `{kind: "detail", rows, total}` below the 2000-row viewport
  threshold or `{kind: "clusters", buckets, total, cell_size}` above
  it.
- `GET /api/catalogue/{id}` — single row.
- `PATCH /api/catalogue/{id}` — partial update with `revert_to_auto`
  support.
- `DELETE /api/catalogue/{id}` — idempotent remove.
- `POST /api/catalogue/{id}/reenrich` — enqueue one row (202
  Accepted).
- `POST /api/catalogue/reenrich` — bulk enqueue.
- `GET /api/catalogue/facets` — cached filter facets (country, ASN,
  city, network). 30-second TTL via `catalogue::facets::FacetsCache`.
- `GET /api/catalogue/stream` — SSE event stream.

Every DTO flows through the `utoipa` → OpenAPI → `openapi-typescript`
pipeline so the frontend client stays in sync.

## Enrichment configuration

```toml
[enrichment.ipgeolocation]
enabled          = true
api_key_env      = "IPGEOLOCATION_API_KEY"
acknowledged_tos = false  # must be true when enabled = true

[enrichment.rdap]
# Enabled by default. Set to false to disable RDAP lookups entirely.
# enabled = true

[enrichment.maxmind]
enabled   = false
city_mmdb = "/var/lib/meshmon/GeoLite2-City.mmdb"
asn_mmdb  = "/var/lib/meshmon/GeoLite2-ASN.mmdb"

[enrichment.whois]
enabled = false
```

An enabled `[enrichment.maxmind]` block with either mmdb path unset is
treated as benign misconfiguration and skipped silently at chain build
time — the operator has toggled the flag before staging the files.
Every other enabled-but-unconstructible provider (missing API key,
feature-gated-out) aborts boot.

## Catalogue invariants

- **Per-row overrides are honoured end-to-end.** Every authoritative
  write appends to `operator_edited_fields`; enrichment reads it on
  merge.
- **Providers are pure.** Persistence happens only in the runner, so
  the lock rule is enforced in one place.
- **Chain order is precedence.** First-writer-wins, earlier beats
  later.
- **The broker never blocks publishers.** Slow SSE clients lag and
  receive a synthetic `lag` frame; they don't backpressure writes.
- **Agents flow through the catalogue.** Agent geo lives on
  `ip_catalogue`; `agents_with_catalogue` resolves it for agent-facing
  queries.

---

# Campaign backend

Measurement campaigns schedule `(source_agent, destination_ip)` pair
probes against a user-defined target set, reuse recent results from
the `measurements` table when available, and publish lifecycle events
for the frontend composer to track.

## Data model

Three tables in `crates/service/migrations/20260420120000_campaigns.up.sql`.

### `measurement_campaigns`

Campaign header row. Columns (selected):

| Column | Type | Notes |
|---|---|---|
| `id` | `UUID` | `gen_random_uuid()` default. |
| `title`, `notes` | `TEXT` | Operator-facing metadata. |
| `state` | `campaign_state` enum | `draft` default; see lifecycle below. |
| `protocol` | `probe_protocol` enum | `icmp` / `tcp` / `udp`. |
| `probe_count` | `SMALLINT` | Probes per dispatched measurement (default 10). |
| `probe_count_detail` | `SMALLINT` | Probes per detail re-run (default 250). |
| `timeout_ms` | `INTEGER` | Per-probe timeout, default 2000. |
| `probe_stagger_ms` | `INTEGER` | Inter-probe stagger, default 100. |
| `force_measurement` | `BOOLEAN` | When `true`, reuse lookup is skipped. |
| `loss_threshold_pct`, `stddev_weight` | `REAL` | Evaluator knobs. |
| `evaluation_mode` | `evaluation_mode` enum | `diversity` or `optimization`. |
| `created_by`, `created_at` | `TEXT` / `TIMESTAMPTZ` | Audit. |
| `started_at`, `stopped_at`, `completed_at`, `evaluated_at` | `TIMESTAMPTZ` | Lifecycle timestamps. |

Indexes:
- `measurement_campaigns_state_started_idx (state, started_at)` — drives
  the scheduler's `active_campaigns` listing (stable RR order).
- `measurement_campaigns_created_by_idx (created_by)` — for
  per-operator filtering.
- `measurement_campaigns_search_idx` — GIN on
  `to_tsvector('simple', title || ' ' || notes)`.

### `campaign_pairs`

One row per `(campaign, source_agent, destination_ip)`. The primary
write surface of the scheduler.

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGSERIAL` | Primary key. |
| `campaign_id` | `UUID` | FK → `measurement_campaigns(id)`, `ON DELETE CASCADE`. |
| `source_agent_id` | `TEXT` | The prober. |
| `destination_ip` | `INET` | Single host; not a wider CIDR. |
| `resolution_state` | `pair_resolution_state` enum | `pending` default. |
| `measurement_id` | `BIGINT` | Nullable FK → `measurements(id)`. |
| `dispatched_at`, `settled_at` | `TIMESTAMPTZ` | Lifecycle. |
| `attempt_count` | `SMALLINT` | Incremented on each dispatch claim. |
| `last_error` | `TEXT` | Most recent error tag. Scheduler-origin: `agent_offline`, `max_attempts_exceeded`, `campaign_stopped`. Writer-origin: `unreachable`, `timeout`, `refused`, `cancelled`, `agent_rejected`. Vocabularies are disjoint so a dashboard filter surfaces origin unambiguously. |

`UNIQUE (campaign_id, source_agent_id, destination_ip)` enforces the
operator-visible pair identity; repeated inserts land via
`ON CONFLICT DO NOTHING` / `DO UPDATE`.

Indexes:
- `campaign_pairs_state_idx (campaign_id, resolution_state)` — drives
  the scheduler's pending-pair claim and `maybe_complete` check.
- `campaign_pairs_settled_idx (campaign_id, settled_at DESC)` — results
  browser ordering.

### `measurements`

One row per settled campaign measurement. Written by the dispatch
writer on every `MeasurementResult` that carries a success outcome;
the 24 h reuse lookup reads the same table. The companion `mtr_traces`
table stores per-round hop arrays for MTR runs and is referenced from
`measurements.mtr_id`. `campaign_pairs.measurement_id` FKs back into
`measurements(id)` so a terminal pair always points at the row it was
settled from.

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGSERIAL` | Primary key. |
| `source_agent_id` | `TEXT` | Prober identity. |
| `destination_ip` | `INET` | Target. |
| `protocol` | `probe_protocol` enum | ICMP / TCP / UDP. |
| `probe_count` | `SMALLINT` | Samples that went into this measurement. |
| `measured_at` | `TIMESTAMPTZ` | `now()` default. |
| `latency_min_ms`, `latency_avg_ms`, `latency_median_ms`, `latency_p95_ms`, `latency_max_ms`, `latency_stddev_ms` | `REAL` | RTT aggregates. |
| `loss_pct` | `REAL` | Loss percentage. |
| `kind` | `measurement_kind` enum | `campaign` default; `detail_ping` / `detail_mtr` for UI re-runs. |
| `mtr_id` | `BIGINT` | Nullable FK → `mtr_traces(id)`; set on MTR settlements. |

`measurements_reuse_idx (source_agent_id, destination_ip, protocol,
probe_count DESC, measured_at DESC)` is tuned for the reuse lookup and
preview-dispatch queries — see the "24 h reuse" section below.

### `mtr_traces`

One row per MTR result. Holds the hop array as JSONB in the same
shape the ingestion pipeline uses for route snapshots so the frontend
consumes both paths without a second deserialiser.

| Column | Type | Notes |
|---|---|---|
| `id` | `BIGSERIAL` | Primary key. |
| `hops` | `JSONB` | Array of `{position, observed_ips, avg_rtt_micros, stddev_rtt_micros, loss_pct}`. |
| `captured_at` | `TIMESTAMPTZ` | `now()` default — when the trace was persisted. |

The writer inserts the trace first, captures the ID, then inserts the
`measurements` row with `mtr_id` set — all inside one transaction so a
measurement never exists without its trace.

### ENUMs

Five Postgres ENUMs created by the migration:

| Type | Values |
|---|---|
| `probe_protocol` | `icmp`, `tcp`, `udp` |
| `campaign_state` | `draft`, `running`, `completed`, `evaluated`, `stopped` |
| `pair_resolution_state` | `pending`, `dispatched`, `reused`, `succeeded`, `unreachable`, `skipped` |
| `evaluation_mode` | `diversity`, `optimization` |
| `measurement_kind` | `campaign`, `detail_ping`, `detail_mtr` |

`campaign::model` mirrors every enum via `#[derive(sqlx::Type)]` with
`rename_all = "snake_case"`, so the Rust side and the database side
share a single source of truth.

### NOTIFY channels

Two channels wake the scheduler ahead of the 500 ms tick fallback.
Both carry the campaign UUID as text, well under pg_notify's 8000-byte
cap, and both are pinned by unit tests that assert the constant value:

- **`campaign_state_changed`** — fired by the
  `measurement_campaigns_notify` trigger on `AFTER INSERT OR UPDATE OF
  state`. Lets the scheduler pick up operator-driven lifecycle changes
  (`start`, `stop`, `edit`, `force_pair`) without waiting for the tick.
- **`campaign_pair_settled`** — fired by the dispatch writer inside
  the settle transaction. Lets the scheduler run `maybe_complete`
  promptly after a batch lands instead of sitting idle until the next
  tick.

Constants live in `campaign::events` (`NOTIFY_CHANNEL` and
`PAIR_SETTLED_CHANNEL`). The scheduler opens one `PgListener` and
calls `listen_all([…])` for both channels; either wake drives a
single `tick_once`. Renaming either channel requires touching the
trigger or writer and the corresponding constant in the same commit.

## Lifecycle

```
      ┌──────────────────── edit-delta re-run ─────────────┐
      │                                                    ▼
  draft ── start ──▶ running ─ maybe_complete ─▶ completed ── start evaluation ──▶ evaluated
                         │                          │                                    │
                         │                          └─── edit-delta re-run ──────────────┤
                         │                                                               │
                         └── stop ─▶ stopped ─── edit-delta re-run ────────────▶ running │
                                                                                         │
                                                                                         ▼
                                                                                      running
```

- `start` (POST `/start`): `draft → running`; stamps `started_at`.
- `stop` (POST `/stop`): `running → stopped`; stamps `stopped_at`. In
  the same transaction, `pending` pairs flip to `skipped` with
  `last_error = 'campaign_stopped'`. In-flight `dispatched` pairs are
  left alone; the dispatch writer settles them as they land.
- `maybe_complete`: the scheduler ends every tick by checking each
  active campaign; a `running → completed` flip happens iff no pair
  remains in `pending` or `dispatched`. Stamps `completed_at`.
- `edit` (POST `/edit`): a finished campaign (`completed`, `stopped`,
  or `evaluated`) may transition back to `running` via an edit-delta.
  See "Edit-delta semantics" below.
- `force_pair` (POST `/force_pair`): resets one `(source, destination)`
  pair to `pending` and ensures the campaign is `running`.

Every transition routes through `repo::transition_state`, which
issues an UPDATE gated on the expected prior state. A 0-row outcome
surfaces as `RepoError::IllegalTransition` (HTTP 409).

### Edit-delta semantics

`POST /api/campaigns/:id/edit` carries three knobs:

- `remove_pairs`: exact-match `DELETE` on `(source, destination)`.
- `add_pairs`: `INSERT … ON CONFLICT (…) DO UPDATE` — a previously
  `skipped` or terminal pair is reset to `pending` with
  `dispatched_at`, `settled_at`, `attempt_count`, `last_error`, and
  `measurement_id` all cleared.
- `force_measurement`: when `Some(true)`, the sticky
  `measurement_campaigns.force_measurement` flag flips to TRUE and
  every non-delta pair currently in `dispatched`, `reused`,
  `succeeded`, `unreachable`, or `skipped` resets to `pending`. The
  whole campaign re-runs; the 24 h reuse cache is ignored for the
  duration. Including `dispatched` in the reset set is load-bearing
  for late settles: the writer's `resolution_state='dispatched'` gate
  observes the reset and drops the stale settle silently instead of
  clobbering the rerun.

After the delta applies, the campaign transitions back to `running`
and `started_at` is bumped. A row-level `FOR UPDATE` lock on the
campaign protects the delta from racing completion/evaluation.

## Scheduler

`campaign::scheduler::Scheduler` is a single long-lived tokio task,
spawned once per service instance. It owns the dispatch loop; the
dispatcher itself is injected via the `PairDispatcher` trait so tests
can drive the loop with stubs (`NoopDispatcher`, `DirectSettleDispatcher`).
Production wires the RPC-backed `RpcDispatcher` (see "Dispatch
transport" below).

### Wake-up

```
tokio::select! {
    _ = cancel.cancelled() => return,
    recv = listener.try_recv() => …,   // PgListener on both channels
    _ = sleep(self.tick)     => …,     // tick fallback, default 500 ms
}
```

The scheduler opens a dedicated `PgListener` and subscribes to
`campaign_state_changed` and `campaign_pair_settled` via `listen_all`.
Either notification wakes the loop ahead of the periodic tick. If the
listener fails to open (or closes mid-run) the scheduler falls back to
a tick-only loop — a transient listener outage never grounds dispatch
permanently.

### Fair round-robin

At the top of each tick the scheduler reloads the set of `running`
campaigns, ordered by `started_at ASC` for stable rotation. For every
active agent (from the `AgentRegistry` snapshot, filtered to
`last_seen_at` within `target_active_window`), it walks the campaigns
starting one past the persisted cursor. The first campaign that yields
any dispatch (fresh, reuse settlement, or rate-limit backoff) sets the
cursor and the loop moves on to the next agent.

The cursor is preserved across ticks. An empty pass leaves the cursor
untouched so the next tick picks up where this one stopped. With N
active campaigns and M active agents, every campaign gets one shot per
agent per tick — fairness is measured in batches, not in pairs.

### Per-destination token buckets

A `moka::future::Cache<IpAddr, Arc<Mutex<Bucket>>>` holds a leaky-bucket
rate limiter keyed on the destination IP. Bucket capacity is
`per_destination_rps` (default 2); each second the bucket refills to
full. The cache `time_to_idle` is 60 s — a destination that stops
receiving traffic expires out of the cache and re-enters at full
capacity the next time the scheduler sees it.

If a batch claims pairs that cannot draw a token, the pair is reverted
to `pending` (resolution_state reset, `dispatched_at` cleared,
`attempt_count` decremented by 1) so a later tick retries it. The
revert is in-line with the claim in the same tick.

### Chunking and claim

`repo::take_pending_batch` runs
`UPDATE … FROM (SELECT … FOR UPDATE SKIP LOCKED)` to atomically claim
up to `chunk_size` pending pairs for `(campaign, source_agent)`,
flipping them to `dispatched` and incrementing `attempt_count`. Two
concurrent tick paths can never double-claim a row; crashed in-flight
rows stay `dispatched` until an operator `force_pair` or
`force_measurement` reset lands (the dispatch writer is the only
terminal-state authority for `dispatched` rows).

### `maybe_complete` and safety sweep

At the end of every tick the scheduler:

1. Calls `repo::maybe_complete(campaign_id)` for each active campaign.
   This flips `running → completed` atomically iff no pair remains in
   `pending` or `dispatched`.
2. Calls `repo::expire_stale_attempts(max_pair_attempts)`, which flips
   any `pending` pair whose `attempt_count >= max_pair_attempts` to
   `skipped` with `last_error = 'max_attempts_exceeded'`. This is the
   safety net for pairs the dispatcher keeps failing.

## Dispatch transport

Production wires `RpcDispatcher` into the scheduler's `PairDispatcher`
seam. Each batch `(campaign, agent, pending-pairs)` the scheduler
hands the dispatcher flows through the pipeline below; test harnesses
swap the implementation for `NoopDispatcher` or `DirectSettleDispatcher`
without touching scheduler code.

### Wire protocol

Dispatch routes through a reverse-tunnel-backed gRPC call:
`AgentCommand.RunMeasurementBatch(RunMeasurementBatchRequest)` returns
`stream MeasurementResult`. Each pair emits exactly one result
correlated by `pair_id`; dropping the client stream cancels the batch
on the agent (HTTP/2 `CANCEL` propagates to the handler's
`CancellationToken`).

`RunMeasurementBatchRequest` carries per-campaign knobs (`protocol`,
`probe_count`, `timeout_ms`, `probe_stagger_ms`) plus a
`MeasurementKind` selector (`LATENCY` | `MTR`). The dispatcher picks
`MTR` when `probe_count == 1` (detail-MTR re-runs force
`probe_count=1`) and `LATENCY` otherwise — an explicit
`measurement_kind` on `PendingPair` will supersede this heuristic once
detail-MTR campaigns are a first-class campaign type.

### Failure-code mapping

Success results carry a full latency (or MTR hop) summary that the
writer persists. Failure results funnel through
`writer::map_failure_code`, which tags `campaign_pairs.last_error` and
picks the terminal `resolution_state`:

| Failure code | `last_error` tag | `resolution_state` |
|---|---|---|
| `NO_ROUTE` | `unreachable` | `unreachable` |
| `TIMEOUT` | `timeout` | `skipped` |
| `REFUSED` | `refused` | `skipped` |
| `CANCELLED` | `cancelled` | `skipped` |
| `AGENT_ERROR` / `UNSPECIFIED` | `agent_rejected` | `skipped` |

The writer's tag vocabulary is disjoint from the scheduler's
(`agent_offline`, `max_attempts_exceeded`, `campaign_stopped`), so an
operator filtering `last_error` on a dashboard sees origin without
ambiguity.

### Per-agent concurrency

Each agent advertises an optional `campaign_max_concurrency` on
`RegisterRequest`; the Register handler persists it on
`agents.campaign_max_concurrency` and zero is rejected at the handler.
On each dispatch the `RpcDispatcher` reads the effective value from
the registry snapshot (override → cluster default →
`[campaigns].default_agent_concurrency`) and acquires a permit on a
per-agent `tokio::sync::Semaphore` sized to match. The semaphore is
cached in a `DashMap` keyed on agent id and rebuilt when the
effective value changes, so an operator tightening the cap takes
effect on the next dispatch without a restart. The agent enforces the
same value inside `AgentCommandService`; an overflow batch returns
`Status::resource_exhausted`, which the dispatcher maps to
`rejected_ids` without settling any pair.

### Per-destination rate limit

A process-wide `moka::future::Cache<IpAddr, Arc<Mutex<Bucket>>>` caps
per-destination request rate. Bucket capacity is
`[campaigns].per_destination_rps` (default 2); each whole second
refills the bucket to full. Cache `time_to_idle` is 60 s so a
destination that stops receiving traffic expires out of the cache.
Pairs that cannot draw a token join `DispatchOutcome::rejected_ids`
and the scheduler reverts them to `pending` on the next tick.

### Writer pipeline and late-settle idempotency

The writer owns the per-result settle transaction:

1. On success, INSERT INTO `measurements` with the latency / loss
   summary.
2. On MTR, INSERT INTO `mtr_traces` first, then INSERT INTO
   `measurements` with `mtr_id` set.
3. On failure, skip the `measurements` insert — the failure tag plus
   target `resolution_state` come from `map_failure_code`.
4. UPDATE `campaign_pairs` SET `resolution_state`, `measurement_id`,
   `settled_at`, `last_error`, **gated on
   `resolution_state = 'dispatched'`**.
5. `SELECT pg_notify('campaign_pair_settled', campaign_id::text)`.

The state predicate is load-bearing. A concurrent operator action
(`apply_edit{force_measurement=true}`, `force_pair`) can flip a
`dispatched` row back to `pending` between claim and settle; without
the gate, a late-arriving result would clobber the reset. The 0-row
UPDATE is the silent-drop path — the writer returns
`SettleOutcome::RaceLost` and `RpcDispatcher` treats it as neither
dispatched nor rejected. A separate `SettleOutcome::MalformedNoOutcome`
covers the "agent sent a result with no `outcome` field" protocol
violation, which the dispatcher reverts via `rejected_ids` so the pair
does not strand in `dispatched`.

### `DispatchOutcome`

The scheduler reverts both revert fields to `pending` on the next
tick. `rate_limited_ids` additionally decrements `attempt_count` so a
pre-RPC throttling decision does not burn retry budget.
`skipped_reason` is set only when the whole batch failed before any
pair streamed:

| Field | Population |
|---|---|
| `dispatched` | Count of pairs whose result streamed back and whose writer settle returned `SettleOutcome::Settled`. |
| `rejected_ids` | Pairs whose result never arrived, pairs that hit a mid-stream RPC error, pairs whose writer settle returned `MalformedNoOutcome`, pairs whose writer settle errored. Scheduler reverts to `pending` **without** `attempt_count--`. |
| `rate_limited_ids` | Pairs that lost the dispatcher's per-destination bucket draw. Scheduler reverts to `pending` AND decrements `attempt_count`. |
| `skipped_reason` | `"agent_unreachable"`, `"rpc_error:<code>"`, `"rate_limited"` (bucket consumed every pair), `"semaphore_closed"`. |

Agent-reported failures (`NO_ROUTE`, `TIMEOUT`, etc.) are **settled**
by the writer — they are not rejections and do not feed
`rejected_ids`.

## Agent one-off prober

The agent-side `CampaignProber` is `crates/agent/src/probing/oneshot.rs::OneshotProber`.
Per batch, it spawns one tokio task per pair, builds a `trippy_core::Tracer`
from the request knobs, runs the tracer under `spawn_blocking`, and
emits a `MeasurementSummary`, `MtrTraceResult`, or `MeasurementFailure`
to the response stream. All four campaign protocols (ICMP / TCP / UDP /
MTR) route through the same builder matrix; no forks of the continuous
prober.

### Trippy builder matrix

`build_oneshot_config(kind, protocol, req, dest_ip, tcp_port, udp_port)`
returns a fresh `trippy_core::Builder`:

| Kind | Protocol | `max_rounds` | `min/max_round_duration` | `read_timeout` | `grace` | TTL | `port_direction` | `trace_identifier` |
|---|---|---|---|---|---|---|---|---|
| `LATENCY` | `ICMP` | `probe_count` | `probe_stagger_ms` | `timeout_ms` | 500 ms | 1..=32 | default | `next_trace_id()` |
| `LATENCY` | `TCP` | `probe_count` | `probe_stagger_ms` | `timeout_ms` | 500 ms | 1..=32 | `FixedDest(port)` | unset |
| `LATENCY` | `UDP` | `probe_count` | `probe_stagger_ms` | `timeout_ms` | 500 ms | 1..=32 | `FixedDest(port)` | unset |
| `MTR` | any | 1 | 0 ms / 30 s | 30 s | 500 ms | 1..=32 | protocol default / `FixedDest(port)` | ICMP only |

MTR pins a single round regardless of `probe_count` and uses a
hard-coded 30 s round timeout. The LATENCY `read_timeout` equals
`req.timeout_ms`; setting `max_round_duration = min_round_duration`
pins the probe cadence so trippy emits no internal jitter.

### Loss predicates

- **ICMP LATENCY** — success iff `target_hop.total_recv() > 0` within
  the per-probe `read_timeout`; silent batches surface as
  `MeasurementFailureCode::TIMEOUT`. Per-probe RTTs come from
  `target_hop.samples()`; the `MeasurementSummary` carries
  min / avg / median / p95 / max / stddev and `loss_pct`.
- **TCP LATENCY** — any destination reply counts as success.
  Trippy 0.13 collapses SYN/ACK and RST replies into
  `total_recv` at the hop level and exposes no per-probe distinction
  on the public `Hop` surface, so the oneshot prober cannot emit an
  explicit `REFUSED` failure today; operators that need to
  distinguish an open port from a refused port rely on latency
  patterns or on the continuous TCP prober's per-probe telemetry.
- **UDP LATENCY** — success iff the destination replied (service
  response OR ICMP Port-Unreachable from the destination IP itself);
  ICMP Time-Exceeded from intermediate hops do not inflate the
  counter because trippy accrues those on lower-TTL hops.
- **MTR** — always emits a `MtrTraceResult`, even against unreachable
  destinations. The single round is dense-packed over
  `[1..=target_reached_ttl]`: every hop with `total_recv > 0`
  contributes one `HopSummary` with its observed IPs (frequency 1.0
  per unique address, single-round), `avg_rtt_micros` derived from
  `best_ms`, `stddev_rtt_micros = 0`, and `loss_pct = 0`. Silent TTLs
  pad with `observed_ips: []`, `loss_pct = 1.0`, and zero RTT. A
  completely silent trace surfaces as `MeasurementFailureCode::TIMEOUT`
  instead of a zero-length MTR result so the writer's `last_error`
  vocabulary stays consistent with LATENCY paths.

### Concurrency

`OneshotProber` owns an independent `tokio::sync::Semaphore` sized from
the cluster-wide `campaign_max_concurrency` cap (same value the
`AgentCommandService`'s outer RPC semaphore uses). The continuous pool's
`MESHMON_ICMP_TARGET_CONCURRENCY` is unaffected — campaign probes
cannot consume continuous permits and vice versa. The combined
ceiling of `continuous_cap + campaign_cap` budgets the tokio blocking
thread pool, which defaults to 64 threads; operators who raise either
cap should validate the combined load.

### Cancellation

The gRPC stream drop propagates to a `CancellationToken` the per-pair
task selects on. On cancel, the task drops the `Arc<Tracer>` (closing
the raw socket) and waits up to 1 s for the blocking `Tracer::run()`
to unwind; if the blocking thread misses the window, the handle is
dropped anyway — the socket is already closed and a stale blocking
thread expires on its next round boundary. Cancelled pairs emit
`MeasurementFailureCode::CANCELLED`.

A wall-clock safety net caps each LATENCY pair at
`probe_count * (stagger_ms + timeout_ms) + 5 s` and MTR pairs at
35 s; tripping the safety net emits `MeasurementFailureCode::TIMEOUT`.

### Shared-resource audit

| Resource | Owner | Coexistence strategy |
|---|---|---|
| ICMP echo identifier | `IcmpClientPool::allocate_id()` (continuous reachability prober) | Oneshot does not allocate. Raw-socket ICMP campaigns use trippy's `trace_identifier` as the echo identifier, which comes from `probing::next_trace_id()`. `surge-ping` and `trippy-core` keep independent reply dispatchers, so identifier overlap cannot cross-contaminate. |
| UDP nonce | per-target `nonce_counter: u32` in the continuous UDP dispatcher | Distinct wire protocol. Continuous UDP uses the meshmon secret-echo handshake; campaign UDP uses trippy traceroute probes which the meshmon listener rejects at the secret gate. |
| Trippy trace id | `probing::next_trace_id()` — process-wide monotonic non-zero `AtomicU16`, randomly seeded | **Shared between continuous MTR and campaign tracers by design.** One allocator, one sequence; uniqueness is guaranteed by construction. |
| TCP/UDP source port | OS ephemeral allocator | Kernel-owned; no application-level collision. |

A defensive agent-side counter
(`ONESHOT_PROBE_COLLISIONS_TOTAL`) mirrors the continuous
`CROSS_CONTAMINATION_TOTAL` so operators can assert both stay at 0.
Coexistence tests spawn a continuous trippy tracer and an oneshot
tracer against the same destination and confirm the invariant.

## 24 h reuse

Before dispatching a batch, the scheduler consults the `measurements`
table for a recent compatible row per pair. The lookup is a single
`DISTINCT ON` SQL statement:

```sql
SELECT DISTINCT ON (r.source_agent_id, r.destination_ip_str)
       r.pair_id, m.id AS measurement_id
  FROM requested r
  JOIN measurements m
    ON m.source_agent_id = r.source_agent_id
   AND m.destination_ip  = r.destination_ip_str::inet
   AND m.protocol        = $4::probe_protocol
   AND m.measured_at     > now() - interval '24 hours'
 ORDER BY r.source_agent_id,
          r.destination_ip_str,
          m.probe_count DESC,
          m.measured_at DESC
```

The `DISTINCT ON` key is `(source_agent_id, destination_ip, protocol)`.
Ordering prefers the highest `probe_count`; `measured_at DESC` is the
tiebreaker when two candidates have the same probe count. Each matched
pair is flipped to `resolution_state = 'reused'` with
`measurement_id` pointing to the matched row and `settled_at = now()`
via `repo::apply_reuse`.

Unmatched pairs fall through to the dispatch path.

When the campaign carries `force_measurement = true`, the reuse lookup
is skipped entirely — the branch in `scheduler::dispatch_for_campaign`
short-circuits past both `resolve_reuse` and `apply_reuse`.

The composer-backed `GET /api/campaigns/:id/preview-dispatch-count`
endpoint uses the same `DISTINCT ON` shape against the campaign's
current pair set to report `(total, reusable, fresh)` — a pure read,
never a write.

## Stop semantics

`POST /api/campaigns/:id/stop` transitions `running → stopped` and, in
the same transaction, flips every `pending` pair to `skipped` with
`last_error = 'campaign_stopped'` and `settled_at = now()`.

In-flight `dispatched` pairs are left alone: the dispatch writer
settles them as they land. A stopped campaign still accepts settlement
writes, so dispatched pairs may flow through to `succeeded`,
`unreachable`, or `skipped` after the stop. The scheduler's
`maybe_complete` does not run on stopped campaigns (stopped is already
terminal from the scheduler's perspective; edit-delta is the only way
back).

## Size guard

`[campaigns] size_warning_threshold` (default 1000) is a purely
advisory soft threshold. The composer frontend surfaces a confirm
dialog when the expected dispatch count (`total` from
`preview-dispatch-count`) exceeds this value. The backend does not
enforce a hard cap — operators may create arbitrarily large campaigns
subject only to memory and disk pressure.

## HTTP surface

All under `/api/campaigns`, all session-authenticated. DTOs live in
`campaign::dto`; every public type derives `utoipa::ToSchema` so the
frontend client stays in lockstep via the
`cargo xtask openapi` → `openapi-typescript` pipeline.

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/campaigns` | Create a draft campaign; seeds the `sources × destinations` pair grid. |
| `GET` | `/api/campaigns` | Filtered list (substring on title/notes, state, `created_by`). |
| `GET` | `/api/campaigns/{id}` | Single row + per-state `pair_counts`. |
| `PATCH` | `/api/campaigns/{id}` | Partial update of editable knobs (title, notes, evaluator params). |
| `DELETE` | `/api/campaigns/{id}` | Idempotent delete; cascades to `campaign_pairs`. |
| `POST` | `/api/campaigns/{id}/start` | `draft → running`. |
| `POST` | `/api/campaigns/{id}/stop` | `running → stopped` with pending-pair flip. |
| `POST` | `/api/campaigns/{id}/edit` | Apply an edit delta and transition back to `running`. |
| `POST` | `/api/campaigns/{id}/force_pair` | Reset one pair and ensure campaign is `running`. |
| `GET` | `/api/campaigns/{id}/pairs` | Paginated pair list; `state` is a comma-separated enum filter. |
| `GET` | `/api/campaigns/{id}/preview-dispatch-count` | Live `(total, reusable, fresh)` against the current pair set. |

Error envelope is `{ "error": "<snake_case_code>" }` to match the
catalogue surface. `RepoError::NotFound` → 404 `not_found`,
`IllegalTransition` → 409 `illegal_state_transition`, anything else
→ 500 `database_error` (detail logged server-side, never surfaced).

## Campaign configuration

```toml
[campaigns]
# Spawn the background scheduler. Default `true` — the agent ships a
# trippy-backed `OneshotProber` that returns real measurements. Set
# `false` to keep the HTTP CRUD and preview endpoints online while
# disabling dispatch (e.g. during an incident or infra migration).
enabled = true
# Composer confirm-dialog threshold on expected dispatch count.
# Advisory only — no hard cap.
size_warning_threshold = 1000
# Scheduler tick fallback in ms. NOTIFY wakes the loop sooner.
scheduler_tick_ms = 500
# Safety-net sweep: `pending` pairs at this attempt count flip to skipped.
max_pair_attempts = 3
# Per-destination-IP token-bucket capacity, refilled once per second.
per_destination_rps = 2
# Cluster-wide per-agent concurrent-measurement cap. An agent's
# `RegisterRequest.campaign_max_concurrency` override (persisted on
# `agents.campaign_max_concurrency`) wins per agent when set.
default_agent_concurrency = 16
# Hard cap on `MeasurementTarget`s in a single RunMeasurementBatch RPC.
# Pairs beyond this cap are dropped at the request-build boundary; the
# scheduler's chunk_size is usually smaller so this is a safety net.
max_batch_size = 50
```

Every knob has a positive-integer guard at config-load time — a zero
value aborts boot with a clear error.

## Self-metrics

The scheduler samples once per tick via `repo::metrics_snapshot`:

- `meshmon_campaigns_total{state}` — gauge per `measurement_campaigns.state`.
- `meshmon_campaign_pairs_total{state}` — gauge per
  `campaign_pairs.resolution_state`.
- `meshmon_campaign_reuse_ratio` — fraction of terminal pairs settled
  by the 24 h reuse window (0.0 when no terminal pairs exist yet).
- `meshmon_campaign_scheduler_tick_seconds` — tick-duration histogram,
  recorded whether the tick body returned Ok or Err.

The snapshot query uses runtime `sqlx::query_as::<_, (T, i64)>` so new
metric aggregates do not require a `.sqlx/` regeneration.

## Campaign invariants

- **State transitions go through `repo::transition_state`.** Every
  UPDATE is gated on the expected prior state and 0-row outcomes
  surface as `IllegalTransition` (HTTP 409). No handler hand-writes an
  unchecked state flip.
- **Two writers own `campaign_pairs.resolution_state`.** The scheduler
  owns claim (`pending → dispatched`), reuse settlements
  (`pending → reused`), and the stale-attempt sweep
  (`pending → skipped`) via `campaign::repo`. The dispatch writer
  owns terminal settle (`dispatched → succeeded|unreachable|skipped`)
  via `campaign::writer::SettleWriter`, gated on
  `resolution_state = 'dispatched'` so concurrent operator resets are
  never clobbered.
- **NOTIFY channel names are load-bearing contracts.** Trigger and
  writer reference the same constants as the scheduler's listener;
  unit tests pin every name.
- **Fair RR at batch granularity.** Each `(campaign, agent)` gets one
  batch per tick; the cursor persists across ticks so one busy campaign
  cannot starve its neighbours.
- **Reuse is a single `DISTINCT ON` SQL statement** keyed on
  `(source_agent_id, destination_ip, protocol)` and preferring the
  highest `probe_count`. Skipped entirely when `force_measurement`.

## See also

- [User guide](user-guide.md) — operator workflow.
- [Runbook](../runbook.md) — operational response.
- `crates/service/src/campaign/README.md` — module layout + file
  responsibilities.
