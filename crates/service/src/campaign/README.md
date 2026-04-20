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
- Per-destination rate limit lives on both the scheduler and
  `RpcDispatcher` as `moka::future::Cache<IpAddr, Bucket>`; cache TTL
  is 60 s idle on each side.
- Fair round-robin cursor is preserved across ticks and only advances
  when a batch actually dispatches (empty passes leave it where it was).

## NOTIFY channels

- `campaign_state_changed` — fired by the
  `measurement_campaigns_notify` trigger on lifecycle changes.
- `campaign_pair_settled` — fired by `SettleWriter` inside the settle
  transaction on every successful terminal UPDATE.

Both payloads are the campaign UUID as text. Constants live in
`events.rs`; a unit test pins each name. The scheduler listens on
both via `PgListener::listen_all`.

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
- `dto.rs` — wire DTOs; `utoipa::ToSchema` on every public type.
- `handlers.rs` — axum handlers for every campaign HTTP endpoint.
