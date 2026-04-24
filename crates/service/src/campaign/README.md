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
- Fair round-robin cursor is preserved across ticks and only advances
  when a batch actually dispatches (empty passes leave it where it was).

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
  predicate (diversity / optimization), serialises to the JSONB shape
  persisted in `campaign_evaluations`.
- `baseline_vm.rs` — fetch-and-archive helper called by the evaluate
  handler before the evaluator runs. Queries VictoriaMetrics for the
  campaign's agent-to-agent RTT / loss baselines over a short lookback
  window, archives the samples as `measurements` rows tagged
  `source='archived_vm_continuous'`, and upserts the matching
  `campaign_pairs` rows so the evaluator's existing
  join-by-measurement-id machinery picks them up. The evaluator no
  longer requires active campaign probes between agents — continuous
  mesh data is authoritative.
- `handlers.rs` — axum handlers for every campaign HTTP endpoint.

## Evaluation flow

`POST /api/campaigns/{id}/evaluate` drives:

1. Gate: state is `completed` or `evaluated`.
2. `baseline_vm::fetch_and_archive_vm_baselines` — pulls the
   agent-mesh baselines from VM over a 15-minute lookback and archives
   them as raw `measurements` rows. 503 on VM misconfig or upstream
   failure.
3. `repo::measurements_for_campaign` — assembles the pure evaluator's
   input from the now-populated tables.
4. `eval::evaluate` — scores transit candidates against the baselines.
5. `repo::write_evaluation` — persists the DTO + flips state to
   `evaluated`.

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
