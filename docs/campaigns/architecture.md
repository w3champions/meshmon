# Campaigns — Architecture

Architecture reference for the campaigns subsystem. Operator-facing usage lives in [`user-guide.md`](user-guide.md).

## Purpose

Campaigns use the meshmon agent fleet to run **one-off** latency / loss / MTR measurements against arbitrary destination IPs, and rank those destinations as potential transit servers for the mesh. A campaign asks "given mesh agents A and B, is some candidate X such that `A → X → B` beats the route we already use?"

Campaigns are orthogonal to continuous probing: they share the transport and the probe libraries, but never run on a schedule and never write to VictoriaMetrics.

## Data flow

```
                 operator browser
                        │
           ┌────────────┼─────────────┐
         /catalogue  /campaigns    /history/pair
           │            │             │
           ▼            ▼             ▼
┌─────────────────────────────────────────────────┐
│            meshmon-service (axum)               │
│ ┌──────────────┐  ┌──────────────┐ ┌──────────┐ │
│ │ catalogue/   │  │ campaign/    │ │ history  │ │
│ │  enrichment  │  │  scheduler   │ │   api    │ │
│ └──────┬───────┘  └───────┬──────┘ └────┬─────┘ │
│        │                  ▼             │       │
│        │         campaign/dispatch ────▶│       │
│        ▼                  ▼             ▼       │
│ ┌─────────────────────────────────────────────┐ │
│ │ Postgres                                    │ │
│ │   ip_catalogue                              │ │
│ │   measurement_campaigns  campaign_pairs     │ │
│ │   measurements  mtr_traces                  │ │
│ │   campaign_evaluations                      │ │
│ └─────────────────────────────────────────────┘ │
└─────────────────────────┬───────────────────────┘
                          │ reverse tunnel
                          ▼
                   ┌─────────────┐
                   │ meshmon-    │
                   │  agent      │
                   │             │
                   │  oneshot    │◀── AgentCommand.RunMeasurementBatch
                   │  (trippy)   │──▶ stream<MeasurementResult>
                   └─────────────┘
```

Dispatch rides on top of the existing reverse tunnel — campaigns add one server-streaming RPC to `AgentCommand` rather than opening a second long-lived stream.

## IP catalogue

`ip_catalogue` is the authoritative record for every IP the system knows about. Every agent registration and every catalogue paste lands here. Rows carry identity fields (`ip`, `display_name`, `website`, `notes`) and enrichment fields (`city`, `country_code`, `country_name`, `latitude`, `longitude`, `asn`, `network_operator`). `enrichment_status` is `pending`, `enriched`, or `failed`.

### Overrides

`operator_edited_fields TEXT[]` marks fields that must never be overwritten by the enrichment chain. Both operator UI edits and agent self-report on `AgentApi.Register` append to this array: an agent's self-reported geo is authoritative because the agent's config was set by someone who knows where it physically lives.

### Enrichment

Enrichment is a background pipeline of pluggable providers, run in order and applying per-field first-writer-wins while skipping any field in `operator_edited_fields`:

1. `ipgeolocation` — primary geo / ASN / network operator. Subject to the provider's free-tier quota.
2. `rdap` — ASN / netname / country fallback via `icann-rdap-client`.
3. `maxmind-geolite2` — self-hosted geo, opt-in.
4. `whois` — legacy netname fallback, opt-in.

A single job runner drains a bounded mpsc queue. Jobs arrive from catalogue creates, agent registrations, and operator-triggered re-enrich actions. The runner broadcasts `ip_catalogue_updated` events over SSE so the UI reflects live progress.

### Agent cross-reference

The `agents` table carries runtime fields only (`id`, `display_name`, `tcp_probe_port`, `udp_probe_port`, `agent_version`, `registered_at`, `last_seen_at`, `ip`, `location` free text). Geographic coordinates live in `ip_catalogue`. The `agents_with_catalogue` view left-joins the two tables on `ip`, feeding both the operator source filter and the agents page.

## Measurements

`measurements` stores one row per observation: a `(source_agent_id, destination_ip, protocol, probe_count, measured_at)` with aggregated latency stats, `loss_pct`, an optional link to `mtr_traces(id)`, and a `kind` discriminator (`campaign`, `detail_ping`, `detail_mtr`). `mtr_traces` holds hops as JSONB.

### 24-hour reuse

Dispatch always checks for a prior measurement within the last 24 hours before sending probes:

```sql
SELECT id FROM measurements
 WHERE source_agent_id = $1 AND destination_ip = $2 AND protocol = $3
   AND measured_at > now() - interval '24 hours'
 ORDER BY probe_count DESC, measured_at DESC
 LIMIT 1;
```

`probe_count DESC` is deliberate: detail measurements (250 probes) dominate regular measurements (10 probes) when both are present. A hit attributes the existing row to the campaign pair (`resolution_state='reused'`) and skips dispatch entirely. `force_measurement` at campaign or per-pair level bypasses the lookup.

## Campaigns

A `measurement_campaigns` row holds operator-facing metadata (`title`, `notes`) and every probing / evaluation knob that applies to the campaign: `protocol`, `probe_count`, `probe_count_detail`, `timeout_ms`, `probe_stagger_ms`, `loss_threshold_pct`, `stddev_weight`, `evaluation_mode`, `force_measurement`. No service-wide defaults shadow these — the DB column defaults are the only defaults.

`campaign_pairs` contains one row per `(campaign × source_agent × destination_ip × kind)` with a `resolution_state` that moves through `pending → dispatched → succeeded | reused | unreachable | skipped`. `kind` is `campaign` (baseline measurements from the original dispatch), `detail_ping`, or `detail_mtr` (follow-up detail measurements).

### Lifecycle

```
  ┌─────────┐ Start   ┌─────────┐ all pairs     ┌──────────┐
  │  Draft  │────────▶│ Running │───settled────▶│ Completed│
  └─────────┘         └────┬────┘               └────┬─────┘
                           │ Stop                    │ Evaluate
                           ▼                         ▼
                      ┌─────────┐               ┌───────────┐
                      │ Stopped │               │ Evaluated │
                      └─────────┘               └─────┬─────┘
                                                      │ Edit (delta)
                                                      ▼
                                              (back to Running)
```

Partial failures do not fail the campaign: offline sources and unreachable destinations simply terminate their individual pairs. Completion requires every pair to reach a terminal state, not every pair to succeed. Editing a terminal-state campaign computes the delta — added and removed pairs only — and returns the row to `Running`.

## Scheduling and dispatch

The `campaign/scheduler` task runs a 500 ms tick augmented by `LISTEN` on Postgres NOTIFY for `campaign_pairs` state changes. On each wake:

1. Resolve active agents (`last_seen_at` within the registry's active window).
2. Walk active campaigns in round-robin order (sorted by `started_at` with a monotonic cursor — new campaigns slot at the end, cursor does not reset).
3. For each agent with free capacity, ask the next campaign for a batch via `take_next_batch`, which enforces three rate limits at once:
   - Per-agent concurrency — the effective cap is the agent's `campaign_max_concurrency` (proto3 optional on `RegisterRequest`) or the cluster default.
   - Per-destination ingress — token bucket keyed by `destination_ip`, default 2 req/s across all agents.
   - Batch size — hard cap 50 pairs per RPC.

The `campaign/dispatch` worker owns one task per active agent. It calls `AgentCommandClient::run_measurement_batch` over the tunnel `Channel` registered by the reverse tunnel manager, consumes the server-streaming `MeasurementResult`s, and persists each into `measurements` + optionally `mtr_traces` with the `campaign_pairs.measurement_id` attribution in the same transaction.

Stop is implemented by flipping `state='stopped'`. The scheduler drops the campaign from the rotation on its next tick; in-flight batches drain naturally because tonic's stream lifetime keeps the probe work alive until results flush. Dropping the gRPC stream, in turn, propagates cancellation to the agent if needed.

## Agent one-off prober

`probing/oneshot.rs` serves every `RunMeasurementBatch` call. It is the single code path for campaign probing and uses `trippy-core` for all protocols — ICMP, TCP, UDP, MTR — because destinations are arbitrary third-party IPs without meshmon listeners. trippy reaches such destinations natively (ICMP echo for ICMP, TCP handshake for TCP, ICMP Port-Unreachable / service reply for UDP).

Per pair, the prober builds a `trippy_core::Tracer` from the batch's fields:

- `max_rounds(probe_count)` controls repetition.
- `min_round_duration(probe_stagger_ms)` controls intra-measurement pacing.
- `max_round_duration(timeout_ms)` bounds per-probe wait.
- `first_ttl(1) / max_ttl(32)` defines the TTL sweep (MTR takes all hops; latency takes only the destination-reached probe's RTT).

Trace identifiers come from `probing::trippy::next_trace_id()` — a process-wide `AtomicU16` that hands out unique non-zero `u16`s across continuous and campaign tracers alike. No range carving, no partitioning.

Loss semantics:
- **ICMP** — success iff the destination's echo reply arrived within the timeout.
- **TCP** — success iff the destination returned SYN/ACK or RST within the timeout (a closed port is still reachable; silent loss is the only failure mode contributing to `loss_pct`). A pair whose every probe was RST'd emits `MeasurementFailure { REFUSED }` instead of a summary.
- **UDP** — success iff the destination replied (service response or ICMP Port-Unreachable from the destination IP) within the timeout. ICMP Time-Exceeded from intermediate hops does not count, matching trippy's destination-reached predicate.

## Evaluation

`campaign/evaluator` runs on demand (Evaluate button). For every `(A, B, X)` where A and B are meshmon agents with a direct measurement and X has measurements from both A and B:

- `direct_rtt = mean(A→B)`, `transit_rtt = mean(A→X) + mean(X→B)`. Symmetry is assumed — the mesh never measures `X → A`, and cross-Atlantic asymmetry is a documented tradeoff.
- `stddev_weight · stddev` is added as a penalty to both sides; unstable routes lose ground.
- Compounded `axb_loss = 1 - (1 - loss(A→X))(1 - loss(X→B))` must be at or below `loss_threshold_pct`, as must `direct_loss`.
- The qualifying predicate depends on `evaluation_mode`:
  - `diversity` — transit beats direct.
  - `optimization` — transit beats both direct and every other `A → Y → B` transit via existing mesh agents `Y` for which the campaign has the necessary measurements.

Results are serialised into a single `campaign_evaluations` row per campaign (one-to-one, overwritten on every Evaluate). Per-candidate detail lives in a JSONB blob; the single-row shape keeps the table small and cache-friendly.

Agents can appear as candidates. In `optimization` mode an agent Y naturally never qualifies against itself (it's already in the baseline); in `diversity` mode it can, badged "mesh member — no acquisition needed".

## Detail measurements

A detail measurement refines a specific pair with an MTR trace plus a 250-probe latency run in the campaign's protocol. Triggered from the results UI with three scopes (all pairs, good candidates only, individual pair), it inserts new `campaign_pairs` rows with `kind ∈ {detail_ping, detail_mtr}` and `force_measurement=true`, returning the campaign to `Running` until the new pairs settle.

Detail rows never feed the evaluator's baseline/candidate matrix — that remains strict `kind=campaign`. They appear in the Raw tab and in the historic pair view, and they dominate regular measurements for the 24 h reuse cache.

## HTTP surface

All under `/api/`, all session-authenticated.

- `catalogue` — CRUD, bulk paste, re-enrich (single and bulk), facets (for filter previews), SSE stream.
- `campaigns` — CRUD, lifecycle transitions (`start`, `stop`, `edit`, `force_pair`), pair listing, `preview-dispatch-count`, `evaluate`, `detail`, `evaluation`, SSE stream.
- `history/{sources,destinations,measurements}` — historic pair view.

All types flow through the `utoipa` → OpenAPI → `openapi-typescript` pipeline, so the frontend client is always in sync.

## Frontend routes

- `/catalogue` — browse and edit, with table and map views and the shared filter component.
- `/campaigns` — list.
- `/campaigns/new` — composer, with side-by-side staging and target panels, paste-many input, and live size preview.
- `/campaigns/:id` — results browser. Four tabs: Candidates (default, candidate-first ranking), Pairs (pivoted), Raw, Evaluation settings.
- `/history/pair` — pick a (source, destination), see latency / loss / MTR over time.

The map uses the existing meshmon map integration; draw-and-select builds on top with `leaflet-geoman` plus `@turf/boolean-point-in-polygon`.

## Configuration

```toml
[enrichment.ipgeolocation]
enabled = true
api_key_env = "IPGEOLOCATION_API_KEY"
acknowledged_tos = false

[enrichment.rdap]
enabled = true

[enrichment.maxmind]
enabled = false
city_mmdb = "/var/lib/meshmon/GeoLite2-City.mmdb"
asn_mmdb  = "/var/lib/meshmon/GeoLite2-ASN.mmdb"

[enrichment.whois]
enabled = false

[campaigns]
size_warning_threshold = 1000
scheduler_tick_ms      = 500
max_pair_attempts      = 3

[campaigns.rate_limits]
default_agent_concurrency = 16
per_destination_rps       = 2.0
max_batch_size            = 50

[agent]
# campaign_max_concurrency = 32   # unset = follow cluster default
```

Per-campaign knobs (`probe_count`, `probe_count_detail`, `timeout_ms`, `probe_stagger_ms`, `loss_threshold_pct`, `stddev_weight`, `evaluation_mode`, `force_measurement`) live on the campaign row. The service will refuse to start if `ipgeolocation.enabled` is true without `acknowledged_tos = true`.

## Observability

Exposed on the existing `/metrics/prometheus`:

| Metric | Type | Purpose |
|---|---|---|
| `meshmon_campaigns_total{state}` | gauge | campaign count per state |
| `meshmon_campaign_pairs_total{state}` | gauge | pair count per state |
| `meshmon_campaign_reuse_ratio` | gauge | fraction of pairs satisfied by the 24 h cache |
| `meshmon_scheduler_tick_seconds` | histogram | scheduler latency |
| `meshmon_campaign_batches_total{agent,kind,outcome}` | counter | dispatched batches |
| `meshmon_campaign_batch_duration_seconds{agent,kind}` | histogram | per-batch wall-clock |
| `meshmon_campaign_pairs_inflight{agent}` | gauge | in-flight pairs per agent |
| `meshmon_campaign_dest_bucket_wait_seconds` | histogram | time spent waiting for a per-destination token |
| `meshmon_campaign_probe_collisions_total` | counter | replies observed outside their tracer's expected identifier; always zero in healthy deployments |

## Invariants

- **Continuous probing stays untouched.** Campaign code reuses the probe libraries and the shared trace-id allocator but does not alter continuous probers' state machines or emitters.
- **One trace-id allocator.** Every trippy tracer in the process — continuous or campaign — draws from `probing::trippy::next_trace_id()`. There is no second allocator and no range partitioning.
- **Campaign data lives in Postgres only.** VictoriaMetrics is for continuous time series; campaign measurements are one-off and stay out of the metrics pipeline.
- **Overrides are per-field.** Any authoritative write appends to `operator_edited_fields`; re-enrichment always respects that list.
- **Baseline evaluation requires agent-agent pairs.** A campaign with none returns an empty evaluation with a clear message. There are no synthetic baselines.
- **Partial success is normal.** Offline sources and unreachable destinations fail pairs, not campaigns.

## See also

- [User guide](user-guide.md) — operator workflow, plain language.
- [Runbook](../runbook.md) — operational response, general.
