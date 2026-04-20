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
2. **`rdap`** (off by default while the in-tree lookup is a stub; flip
   `[enrichment.rdap] enabled = true` once the real registry wiring
   ships) — free, credential-less registry lookup via
   `icann-rdap-client`. Fills registry-level fields (ASN, network
   operator, country) that `ipgeolocation` did not already supply.
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
- `GET /api/catalogue` — filtered list (country, ASN, network, name,
  IP prefix, bounding box). Capped at 500 rows.
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
# The in-tree provider is a stub today — leave disabled until the real
# wire-up ships. See `rdap_enabled_default` for the reasoning.
enabled = false

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
| `last_error` | `TEXT` | Most recent error tag; e.g. `max_attempts_exceeded`. |

`UNIQUE (campaign_id, source_agent_id, destination_ip)` enforces the
operator-visible pair identity; repeated inserts land via
`ON CONFLICT DO NOTHING` / `DO UPDATE`.

Indexes:
- `campaign_pairs_state_idx (campaign_id, resolution_state)` — drives
  the scheduler's pending-pair claim and `maybe_complete` check.
- `campaign_pairs_settled_idx (campaign_id, settled_at DESC)` — results
  browser ordering.

### `measurements`

Minimal skeleton used by the 24 h reuse lookup. T44 never writes this
table; T45's result-writer populates it. A follow-up dispatch-transport
migration extends the table with `mtr_id` and adds the sibling
`mtr_traces` table plus the FK `campaign_pairs.measurement_id →
measurements(id)`.

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

`measurements_reuse_idx (source_agent_id, destination_ip, protocol,
probe_count DESC, measured_at DESC)` is tuned for the reuse lookup and
preview-dispatch queries — see the "24 h reuse" section below.

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

### NOTIFY trigger

`measurement_campaigns_notify` fires `AFTER INSERT OR UPDATE OF state`
on `measurement_campaigns` and calls
`pg_notify('campaign_state_changed', NEW.id::text)`. The payload is
always the campaign UUID, well under pg_notify's 8000-byte cap. The
scheduler's `PgListener::listen(NOTIFY_CHANNEL)` — `NOTIFY_CHANNEL` is
defined in `campaign::events` — wakes ahead of the 500 ms tick
whenever a state change commits.

The channel name is a load-bearing contract. Renaming it requires
touching the trigger in the migration and
`campaign::events::NOTIFY_CHANNEL` in the same commit; a unit test
pins the constant value to make the coupling explicit.

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
  left alone; the dispatch-layer writer (T45) settles them as-is.
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
  every non-delta pair currently in `reused`, `succeeded`, or
  `unreachable` resets to `pending`. The whole campaign re-runs; the
  24 h reuse cache is ignored for the duration.

After the delta applies, the campaign transitions back to `running`
and `started_at` is bumped. A row-level `FOR UPDATE` lock on the
campaign protects the delta from racing completion/evaluation.

## Scheduler

`campaign::scheduler::Scheduler` is a single long-lived tokio task,
spawned once per service instance. It owns the dispatch loop; the
dispatcher itself is injected via the `PairDispatcher` trait so tests
can drive the loop with stubs (`NoopDispatcher`, `DirectSettleDispatcher`)
and T45 plugs in the real RPC-backed dispatcher.

### Wake-up

```
tokio::select! {
    _ = cancel.cancelled() => return,
    recv = listener.try_recv() => …,   // PgListener on NOTIFY
    _ = sleep(self.tick)     => …,     // tick fallback, default 500 ms
}
```

The scheduler opens a dedicated `PgListener` on `NOTIFY_CHANNEL`; any
`campaign_state_changed` NOTIFY wakes the loop ahead of the periodic
tick. If the listener fails to open (or closes mid-run) the scheduler
falls back to a tick-only loop — a transient listener outage never
grounds dispatch permanently.

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
concurrent tick paths can never double-claim a row; crash-recovery is
the operator's problem only for rows already in-flight (T45's writer
is the settle authority).

### `maybe_complete` and safety sweep

At the end of every tick the scheduler:

1. Calls `repo::maybe_complete(campaign_id)` for each active campaign.
   This flips `running → completed` atomically iff no pair remains in
   `pending` or `dispatched`.
2. Calls `repo::expire_stale_attempts(max_pair_attempts)`, which flips
   any `pending` pair whose `attempt_count >= max_pair_attempts` to
   `skipped` with `last_error = 'max_attempts_exceeded'`. This is the
   safety net for pairs the dispatcher keeps failing.

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

In-flight `dispatched` pairs are left alone: the dispatch-layer writer
(T45) is responsible for settling them as they land. A stopped
campaign still accepts settlement writes, so dispatched pairs may
flow through to `succeeded`, `unreachable`, or `skipped` after the
stop. The scheduler's `maybe_complete` does not run on stopped
campaigns (stopped is already terminal from the scheduler's
perspective; edit-delta is the only way back).

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
# Spawn the background scheduler. Default: false. Keep it off until a
# real dispatcher is wired (T45) — the T44 NoopDispatcher flips pairs
# pending→dispatched but never settles them. HTTP CRUD + preview work
# regardless of this flag.
enabled = false
# Composer confirm-dialog threshold on expected dispatch count.
# Advisory only — no hard cap.
size_warning_threshold = 1000
# Scheduler tick fallback in ms. NOTIFY wakes the loop sooner.
scheduler_tick_ms = 500
# Safety-net sweep: `pending` pairs at this attempt count flip to skipped.
max_pair_attempts = 3
# Per-destination-IP token-bucket capacity, refilled once per second.
per_destination_rps = 2
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
- **The scheduler is the sole writer of `campaign_pairs.resolution_state`
  for T44.** Reuse settlements, dispatch claims, and stale-attempt
  sweeps all route through `campaign::repo`. The dispatch-layer writer
  (T45) is the only other authority and it only writes terminal states.
- **The NOTIFY channel name is a load-bearing contract.** Trigger and
  listener reference the same constant; a unit test pins the name.
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
