# `crates/service/src/campaign/`

Measurement-campaign subsystem.

## Invariants

- `measurement_campaigns.state` and `campaign_pairs.resolution_state` are
  Postgres ENUMs. All state transitions go through
  `repo::transition_state`, which issues an UPDATE gated on the expected
  prior state and surfaces 0-row outcomes as
  `RepoError::IllegalTransition` (→ HTTP 409).
- Two writers own `campaign_pairs.resolution_state`: the scheduler
  task (claim, reuse, stale-attempt sweep via `campaign::repo`) and
  `SettleWriter` (terminal settle from agent-reported results, gated
  on `resolution_state='dispatched'`). Integration tests that
  simulate settlements use `DirectSettleDispatcher`.
- Per-destination rate limit lives on `RpcDispatcher` as a
  `moka::future::Cache<IpAddr, Bucket>` (60 s idle TTL). Bucket-rejected
  pairs flow back to the scheduler via `DispatchOutcome::rate_limited_ids`
  so the scheduler reverts them with `attempt_count--`.
- Each tick snapshots the round-robin cursor once, then fans out one
  async dispatch per active agent via `FuturesUnordered`. Every fan-out
  future independently walks the campaign ring from the shared snapshot
  and breaks on the first campaign with pending work for that agent.
  Ticks do not overlap — a tick completes when its fan-out drains. The
  cursor advances by exactly one slot per tick if any agent dispatched;
  it does not follow the last campaign any individual agent settled on,
  because concurrent agents may resolve different campaigns within the
  same tick. Empty passes leave it where it was.

## NOTIFY channels

- `campaign_state_changed` — fired by the
  `measurement_campaigns_notify` trigger on lifecycle changes.
- `campaign_pair_settled` — fired by `SettleWriter` inside the settle
  transaction on every successful terminal UPDATE.
- `campaign_evaluated` — fired by the `campaign_evaluations_notify`
  trigger on INSERT / UPDATE of `campaign_evaluations`. Drives the
  cross-instance `evaluated` SSE fan-out.

All payloads are the campaign UUID as text. Constants live in
`events.rs`; a unit test pins each name. The scheduler listens on
the first two; the SSE listener listens on all three via
`PgListener::listen_all`.

## Files

- `model.rs` — domain types + `transition_allowed()`.
- `repo.rs` — sqlx queries; every scheduler-side write goes through here.
- `events.rs` — `NOTIFY_CHANNEL` + `PAIR_SETTLED_CHANNEL` constants.
- `dispatch.rs` — `PairDispatcher` trait + stub dispatchers.
- `rpc_dispatcher.rs` — the production `PairDispatcher`; see
  `dispatch.md` for per-call flow.
- `writer.rs` — `SettleWriter`; owns the per-result transaction and
  the writer-origin `last_error` mapping.
- `scheduler.rs` — single-task fair-RR scheduler.
- `broker.rs` — `CampaignBroker` + `CampaignStreamEvent`; broadcast
  fan-out used by the SSE endpoint.
- `listener.rs` — dedicated `PgListener` task that tails
  `campaign_state_changed` + `campaign_pair_settled` and publishes to
  the broker.
- `sse.rs` — `/api/campaigns/stream` handler; subscribes to the broker
  and serializes events as `Event::data` frames.
- `dto.rs` — wire DTOs; `utoipa::ToSchema` on every public type.
- `eval.rs` — pure-function evaluator core. Builds the (A,B,X) triple
  matrix from attributed measurements, applies the mode-specific
  predicate (diversity / optimization), returns the result payload
  for `evaluation_repo` to persist across the relational child tables.
- `evaluation_repo.rs` — owns the `campaign_evaluations` family:
  `insert_evaluation` (atomic writer primitive that fans the evaluator
  output across `campaign_evaluations` + `campaign_evaluation_candidates`
  + `campaign_evaluation_pair_details` +
  `campaign_evaluation_unqualified_reasons` inside the caller's tx),
  `persist_evaluation` (the orchestrator that locks the campaign row,
  inserts, and promotes `completed → evaluated` in one tx), and
  `latest_evaluation_for_campaign` (the read-path that assembles the
  wire DTO from the four tables).
- `handlers.rs` — axum handlers for every campaign HTTP endpoint.

## Evaluation flow

Every `/evaluate` call appends a fresh `campaign_evaluations` row —
the per-campaign UNIQUE is gone, so history accumulates and
`GET /evaluation` picks the latest via `(campaign_id, evaluated_at
DESC)`.

`POST /api/campaigns/{id}/evaluate` drives:

1. Gate: state is `completed` or `evaluated`.
2. `repo::measurements_for_campaign` — assembles the active-probe
   baseline set (declared pairs + `measurements` rows) stamped with
   `DirectSource::ActiveProbe`.
3. `fetch_and_synthesize_vm_baselines` — for every agent→agent pair
   the active-probe set didn't cover, query VictoriaMetrics via
   `vm_query::fetch_agent_baselines` and synthesize
   `AttributedMeasurement` rows stamped `DirectSource::VmContinuous`.
   Silent no-op when `[upstream] vm_url` is unset; any reachable-but-
   failed VM query surfaces as 503 `vm_upstream`.
4. Concatenate synthesized rows **first**, then the active-probe rows,
   so the evaluator's `by_pair` `HashMap::insert` keeps the
   active-probe row when both sources cover the same
   `(source_agent_id, destination_ip)`. The synthesis step additionally
   filters out pairs already covered by active probes, so
   active-probe-wins is enforced at both layers.
5. `eval::evaluate` — scores transit candidates against the combined
   baseline set and stamps each `pair_detail` with the
   `direct_source` from the baseline row it actually used.
6. `evaluation_repo::persist_evaluation` — inserts the parent row +
   every candidate, pair_detail, and unqualified_reason child row
   atomically inside one tx, then promotes campaign state to
   `evaluated`.
7. `evaluation_repo::latest_evaluation_for_campaign` — read-back that
   supplies the handler's response DTO.

VM-sourced rows are ephemeral: they never land in `measurements` and
only live inside the `/evaluate` handler's in-memory input. Only the
`direct_source` enum on every persisted `pair_detail` records which
source fed the baseline.

## SSE event stream

`GET /api/campaigns/stream` is an authenticated Server-Sent Events
endpoint. Clients subscribe once and receive one frame per campaign
lifecycle transition or pair settle, for every campaign in the cluster.

### Event envelope

Every frame is a single-line `data:` payload carrying JSON with a top-level
`kind` discriminant:

| `kind`           | Additional fields                              | Meaning                                                                          |
|------------------|------------------------------------------------|----------------------------------------------------------------------------------|
| `state_changed`  | `campaign_id: uuid`, `state: CampaignState`    | Campaign moved into `state` (handler call or scheduler-driven `running→completed`) |
| `pair_settled`   | `campaign_id: uuid`                            | `SettleWriter` terminally resolved a pair belonging to `campaign_id`             |
| `evaluated`      | `campaign_id: uuid`                            | `campaign_evaluations` INSERT / UPDATE for `campaign_id` (fired by the `campaign_evaluations_notify` trigger; fanned out via `campaign_evaluated`) |
| `lag`            | `missed: u64`                                  | Subscriber fell behind the broker's 512-slot buffer; re-fetch to reconcile       |

The `lag` frame is synthetic — emitted by the SSE handler when the
broadcast receiver returns `Lagged(n)`. A 15 s keep-alive comment keeps
intermediate proxies from idling the connection out.

### Architecture

Neither publisher lives on the HTTP request path. The scheduler flips
`running → completed` autonomously, and the writer fires
`campaign_pair_settled` inside the settle transaction. To fan these
events out to SSE, [`listener.rs`](listener.rs) opens a **dedicated**
`PgListener` (independent of the scheduler's own listener — PostgreSQL
delivers NOTIFY payloads to every `LISTEN`ing connection), resolves
each wake-up, and publishes onto the process-wide
[`CampaignBroker`](broker.rs). Subscribers connect to
`/api/campaigns/stream`; the handler in [`sse.rs`](sse.rs) forwards
every event as an `Event::data` frame. The listener reconnects on
failure with capped 1 s → 30 s backoff; the broker survives the
reconnect so existing SSE clients never have to re-open their streams.
