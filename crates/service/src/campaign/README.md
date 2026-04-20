# `crates/service/src/campaign/`

Measurement-campaign subsystem.

## Invariants

- `measurement_campaigns.state` and `campaign_pairs.resolution_state` are
  Postgres ENUMs. All state transitions go through
  `repo::transition_state`, which issues an UPDATE gated on the expected
  prior state and surfaces 0-row outcomes as
  `RepoError::IllegalTransition` (→ HTTP 409).
- Only the scheduler task writes `campaign_pairs.resolution_state`
  outside of the dispatch-layer writer (T45). Integration tests that
  simulate settlements use `DirectSettleDispatcher`.
- Per-destination rate limit lives on the scheduler's
  `moka::future::Cache<IpAddr, Bucket>`; cache TTL is 60 s idle.
- Fair round-robin cursor is preserved across ticks and only advances
  when a batch actually dispatches (empty passes leave it where it was).

## NOTIFY channel

`campaign_state_changed` carries the campaign UUID as a string payload.
See the trigger in `migrations/20260420120000_campaigns.up.sql` and the
listener in `scheduler::Scheduler::run`.

## Files

- `model.rs` — domain types + `transition_allowed()`.
- `repo.rs` — sqlx queries; every write goes through here.
- `events.rs` — NOTIFY channel constant + explicit publisher helper.
- `dispatch.rs` — `PairDispatcher` trait + stub dispatchers.
- `scheduler.rs` — single-task fair-RR scheduler.
- `dto.rs` — wire DTOs; `utoipa::ToSchema` on every public type.
- `handlers.rs` — axum handlers for every endpoint in spec 02 §7.
