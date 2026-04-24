//! Settlement writer for one-off campaign measurements.
//!
//! Consumes a [`MeasurementResult`] streamed back from the agent and
//! turns it into:
//!   * a row in `measurements` (always, on success / MTR paths),
//!   * optionally a row in `mtr_traces` with `measurements.mtr_id` set,
//!   * an UPDATE of `campaign_pairs.resolution_state` to the appropriate
//!     terminal state, gated on `resolution_state = 'dispatched'` so a
//!     concurrent operator reset is not clobbered by a late settle,
//!   * a `pg_notify('campaign_pair_settled', campaign_id::text)` so the
//!     scheduler runs `maybe_complete` promptly.
//!
//! Every write for a single result lives inside one transaction. If any
//! step fails the whole result is rolled back so measurements never
//! appear "half-attributed" (a `measurements` row without a matching
//! `campaign_pairs.measurement_id` pointer).
//!
//! [`settle`] returns a [`SettleOutcome`] so the caller can distinguish
//! between three disjoint outcomes:
//!   * [`SettleOutcome::Settled`] — the pair was actually updated,
//!   * [`SettleOutcome::RaceLost`] — the `resolution_state='dispatched'`
//!     gate refused the update because a concurrent reset landed first,
//!   * [`SettleOutcome::MalformedNoOutcome`] — the result arrived with
//!     no `outcome` field, which is a protocol violation from the agent.
//!
//! Dispatchers treat `RaceLost` as a silent drop and `MalformedNoOutcome`
//! as a rejection so the scheduler reverts the pair — a malformed result
//! must not leave a pair stranded in `dispatched`.

use super::dispatch::PendingPair;
use super::events::PAIR_SETTLED_CHANNEL;
use super::model::{MeasurementKind, PairResolutionState, ProbeProtocol};
use crate::ingestion::json_shapes::{HopIpJson, HopJson};
use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{HopSummary, MeasurementFailureCode, MeasurementResult};
use serde_json::Value as JsonValue;
use sqlx::{types::ipnetwork::IpNetwork, PgPool};
use tracing::warn;

/// Per-failure-code tag written to `campaign_pairs.last_error`.
///
/// Must NEVER collide with the T44 scheduler-origin tags
/// (`agent_offline`, `max_attempts_exceeded`, `campaign_stopped`).
pub fn map_failure_code(code: MeasurementFailureCode) -> &'static str {
    match code {
        MeasurementFailureCode::Unspecified => "agent_rejected",
        MeasurementFailureCode::NoRoute => "unreachable",
        MeasurementFailureCode::Refused => "refused",
        MeasurementFailureCode::Timeout => "timeout",
        MeasurementFailureCode::Cancelled => "cancelled",
        MeasurementFailureCode::AgentError => "agent_rejected",
    }
}

/// Returns the target terminal `resolution_state` for a given failure tag.
///
/// Real measurement failures map to `Unreachable`; non-measurement
/// terminations map to `Skipped`. The tag is preserved in `last_error`
/// either way so operators keep the origin signal.
///
/// Tag vocabulary (from [`map_failure_code`]):
/// * `"unreachable"`, `"timeout"`, `"refused"` — the probe ran and the
///   destination failed to answer (no route, silent drops, active
///   refuse). These are valid datapoints; packet loss is a signal.
/// * `"cancelled"`, `"agent_rejected"` — the agent aborted before or
///   during the probe (scheduler shutdown, agent-side protocol error).
///   No measurement was attempted.
/// * Any unknown tag defaults to `Skipped` so a future writer-side
///   classification bug can't silently inflate unreachability.
fn state_for_failure_tag(tag: &str) -> PairResolutionState {
    match tag {
        "unreachable" | "timeout" | "refused" => PairResolutionState::Unreachable,
        _ => PairResolutionState::Skipped,
    }
}

/// Convert protocol `HopSummary`s into the JSONB shape stored in
/// `mtr_traces.hops`. We reuse the ingestion `HopJson` struct so the
/// on-disk layout stays identical to `route_snapshots.hops` and the
/// frontend can consume both paths without a second shape.
fn hops_to_jsonb(hops: &[HopSummary]) -> Result<JsonValue, serde_json::Error> {
    let converted: Vec<HopJson> = hops
        .iter()
        .map(|h| HopJson {
            position: h.position,
            // Best-effort decode — the agent side enforces valid
            // 4- or 16-byte IPs, but a malformed payload must not
            // sink the whole settle. Drop unparseable hops silently
            // (they would be rejected by the ingestion validator
            // anyway).
            observed_ips: h
                .observed_ips
                .iter()
                .filter_map(|ip| {
                    meshmon_protocol::ip::to_ipaddr(&ip.ip)
                        .ok()
                        .map(|addr| HopIpJson {
                            ip: addr.to_string(),
                            freq: ip.frequency,
                            hostname: None,
                        })
                })
                .collect(),
            avg_rtt_micros: h.avg_rtt_micros,
            stddev_rtt_micros: h.stddev_rtt_micros,
            loss_pct: h.loss_pct,
        })
        .collect();
    serde_json::to_value(converted)
}

/// Disjoint outcomes of a single [`SettleWriter::settle`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleOutcome {
    /// The pair row was updated; a measurement (and, for MTR, a trace)
    /// was inserted; the `campaign_pair_settled` NOTIFY fired.
    Settled,
    /// The `resolution_state='dispatched'` gate refused the update — a
    /// concurrent operator reset landed first. The whole transaction
    /// rolled back; callers drop this case silently.
    RaceLost,
    /// The agent sent a result with no `outcome` field — protocol
    /// violation. The whole transaction rolled back; callers must treat
    /// this as a rejection so the scheduler reverts the pair rather
    /// than leaving it stranded in `dispatched`.
    MalformedNoOutcome,
}

/// Concrete writer that owns a connection pool.
#[derive(Clone)]
pub struct SettleWriter {
    pool: PgPool,
}

impl SettleWriter {
    /// Construct a writer bound to the given pool. Cheap — clone freely.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Persist one result. See [`SettleOutcome`] for the three possible
    /// non-error returns.
    pub async fn settle(
        &self,
        pair: &PendingPair,
        result: &MeasurementResult,
    ) -> Result<SettleOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        let (measurement_id, pair_end_state, last_error_tag): (
            Option<i64>,
            PairResolutionState,
            Option<&'static str>,
        ) = match &result.outcome {
            Some(Outcome::Success(s)) => {
                // Latency path handles both `campaign` and `detail_ping`
                // dispatches — propagate the pair's kind onto the
                // measurement row rather than hardcoding `campaign`, or
                // detail-ping results would be indistinguishable from
                // baseline data in `measurements.kind`.
                let m_id: i64 = sqlx::query_scalar!(
                    r#"
                    INSERT INTO measurements
                        (source_agent_id, destination_ip, protocol, probe_count,
                         latency_min_ms, latency_avg_ms, latency_median_ms,
                         latency_p95_ms, latency_max_ms, latency_stddev_ms,
                         loss_pct, kind)
                    VALUES ($1, $2, $3::probe_protocol, $4,
                            $5, $6, $7, $8, $9, $10, $11,
                            $12::measurement_kind)
                    RETURNING id
                    "#,
                    pair.source_agent_id,
                    IpNetwork::from(pair.destination_ip),
                    pair.protocol as ProbeProtocol,
                    i16::try_from(s.attempted).unwrap_or(i16::MAX),
                    s.latency_min_ms,
                    s.latency_avg_ms,
                    s.latency_median_ms,
                    s.latency_p95_ms,
                    s.latency_max_ms,
                    s.latency_stddev_ms,
                    s.loss_pct,
                    pair.kind as MeasurementKind,
                )
                .fetch_one(&mut *tx)
                .await?;
                (Some(m_id), PairResolutionState::Succeeded, None)
            }
            Some(Outcome::Failure(f)) => {
                // `f.code` is a raw `i32` on the wire; clamp unknown
                // codes to `Unspecified` so a future enum addition in
                // the agent doesn't corrupt our state machine.
                let code = MeasurementFailureCode::try_from(f.code)
                    .unwrap_or(MeasurementFailureCode::Unspecified);
                let tag = map_failure_code(code);
                (None, state_for_failure_tag(tag), Some(tag))
            }
            Some(Outcome::Mtr(trace)) => {
                let hops_json = hops_to_jsonb(&trace.hops).map_err(|e| {
                    // A serde_json error here is a program bug, not a
                    // recoverable DB error; surface it as a protocol
                    // error so the caller logs and reverts cleanly.
                    sqlx::Error::Protocol(format!("failed to serialize hops: {e}"))
                })?;
                let trace_id: i64 = sqlx::query_scalar!(
                    "INSERT INTO mtr_traces (hops) VALUES ($1) RETURNING id",
                    hops_json,
                )
                .fetch_one(&mut *tx)
                .await?;
                // MTR path: today only `detail_mtr` pairs dispatch as
                // MTR (see `rpc_dispatcher::build_request`), so
                // `pair.kind` is always `DetailMtr` here. Binding the
                // pair's actual kind instead of hardcoding the literal
                // keeps the measurement row honest if a future dispatch
                // policy ever routes a campaign-kind pair through MTR.
                let m_id: i64 = sqlx::query_scalar!(
                    r#"
                    INSERT INTO measurements
                        (source_agent_id, destination_ip, protocol, probe_count,
                         loss_pct, kind, mtr_id)
                    VALUES ($1, $2, $3::probe_protocol, 1,
                            0.0, $4::measurement_kind, $5)
                    RETURNING id
                    "#,
                    pair.source_agent_id,
                    IpNetwork::from(pair.destination_ip),
                    pair.protocol as ProbeProtocol,
                    pair.kind as MeasurementKind,
                    trace_id,
                )
                .fetch_one(&mut *tx)
                .await?;
                (Some(m_id), PairResolutionState::Succeeded, None)
            }
            None => {
                warn!(
                    pair_id = pair.pair_id,
                    "settle called with empty outcome; rejecting as malformed",
                );
                tx.rollback().await?;
                return Ok(SettleOutcome::MalformedNoOutcome);
            }
        };

        // The `AND resolution_state = 'dispatched'` predicate is
        // load-bearing: a concurrent `apply_edit{force_measurement=true}`
        // or `force_pair` can flip a dispatched row back to `pending`
        // between claim and settle. The late settle must be a silent
        // no-op in that race — clobbering the reset would cause the
        // pair to re-run with a stale measurement.
        let updated = sqlx::query!(
            r#"
            UPDATE campaign_pairs
               SET resolution_state = $2::pair_resolution_state,
                   measurement_id   = $3,
                   settled_at       = now(),
                   last_error       = $4
             WHERE id = $1
               AND resolution_state = 'dispatched'
            "#,
            pair.pair_id,
            pair_end_state as PairResolutionState,
            measurement_id,
            last_error_tag,
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if updated == 0 {
            // Pair was reset between claim and settle — drop silently.
            tx.rollback().await?;
            return Ok(SettleOutcome::RaceLost);
        }

        // Fire the NOTIFY inside the same transaction so the scheduler
        // wake-up only lands after the writes are durable; a listener
        // that sees the notification is guaranteed to read the settled
        // row on the next query.
        sqlx::query!(
            "SELECT pg_notify($1, $2::text) AS _notified",
            PAIR_SETTLED_CHANNEL,
            pair.campaign_id.to_string(),
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(SettleOutcome::Settled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_code_mapping_is_exhaustive() {
        // Every known failure code maps to a non-empty tag. A new
        // variant added upstream must flow through this match — if it
        // ever does, rustc flags the panic path first.
        for code in [
            MeasurementFailureCode::Unspecified,
            MeasurementFailureCode::NoRoute,
            MeasurementFailureCode::Refused,
            MeasurementFailureCode::Timeout,
            MeasurementFailureCode::Cancelled,
            MeasurementFailureCode::AgentError,
        ] {
            let tag = map_failure_code(code);
            assert!(!tag.is_empty(), "{code:?} has no tag");
        }
    }

    #[test]
    fn t45_tags_never_collide_with_t44_scheduler_origin_tags() {
        // The writer must not emit any of the scheduler-origin tags or
        // operators lose the origin signal on a dashboard filter.
        let scheduler_tags = ["agent_offline", "max_attempts_exceeded", "campaign_stopped"];
        for code in [
            MeasurementFailureCode::Unspecified,
            MeasurementFailureCode::NoRoute,
            MeasurementFailureCode::Refused,
            MeasurementFailureCode::Timeout,
            MeasurementFailureCode::Cancelled,
            MeasurementFailureCode::AgentError,
        ] {
            let tag = map_failure_code(code);
            assert!(
                !scheduler_tags.contains(&tag),
                "writer tag {tag:?} collides with scheduler-origin vocabulary",
            );
        }
    }

    #[test]
    fn no_route_targets_unreachable_state() {
        let tag = map_failure_code(MeasurementFailureCode::NoRoute);
        assert_eq!(tag, "unreachable");
        assert_eq!(state_for_failure_tag(tag), PairResolutionState::Unreachable);
    }

    /// Exhaustive regression barrier: every `MeasurementFailureCode`
    /// variant must round-trip through `map_failure_code` →
    /// `state_for_failure_tag` to the documented resolution state.
    ///
    /// This is load-bearing — per spec §3.2/§3.3, `timeout` / `refused`
    /// / `unreachable` are real measurement failures (resolve to
    /// `Unreachable`, 100% loss is a signal), while `cancelled` /
    /// `agent_rejected` are non-measurement terminations (resolve to
    /// `Skipped`, no attempt made). A single refactor of the match arm
    /// could silently invert the mapping; this test fails if it does.
    #[test]
    fn failure_code_resolves_to_expected_state() {
        use MeasurementFailureCode::*;
        use PairResolutionState::{Skipped, Unreachable};

        let cases: &[(MeasurementFailureCode, &str, PairResolutionState)] = &[
            (Unspecified, "agent_rejected", Skipped),
            (NoRoute, "unreachable", Unreachable),
            (Refused, "refused", Unreachable),
            (Timeout, "timeout", Unreachable),
            (Cancelled, "cancelled", Skipped),
            (AgentError, "agent_rejected", Skipped),
        ];

        for &(code, expected_tag, expected_state) in cases {
            let tag = map_failure_code(code);
            assert_eq!(tag, expected_tag, "{code:?} tag regressed");
            assert_eq!(
                state_for_failure_tag(tag),
                expected_state,
                "{code:?} (tag {tag:?}) resolution state regressed",
            );
        }
    }
}
