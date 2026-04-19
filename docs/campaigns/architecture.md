# Campaigns — Architecture

Developer reference for the catalogue subsystem that backs the campaigns
feature. Operator-facing workflow lives in [`user-guide.md`](user-guide.md).

The campaigns layer (scheduler, dispatch, evaluator, one-off prober) is a
later subsystem — this document covers the catalogue and the enrichment
pipeline it depends on.

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

## Runner

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

## Event broker and SSE

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

## HTTP surface

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

## Configuration

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

## Invariants

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

## See also

- [User guide](user-guide.md) — operator workflow.
- [Runbook](../runbook.md) — operational response.
- Campaigns scheduler, dispatch, evaluator, and one-off prober — later
  subsystems; this doc is intentionally silent on those surfaces.
