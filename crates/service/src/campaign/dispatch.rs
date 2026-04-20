//! Dispatcher trait + test stubs. Production RPC dispatch is T45.

use super::model::PairResolutionState;
use async_trait::async_trait;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// A single pair ready to be dispatched to an agent.
///
/// Carries enough context (campaign knobs + destination) that the
/// dispatcher does not need to re-query the database; the scheduler is
/// responsible for populating it from a [`PairRow`] plus the owning
/// campaign's configuration.
#[derive(Debug, Clone)]
pub struct PendingPair {
    /// `campaign_pairs.id` of the row being dispatched.
    pub pair_id: i64,
    /// Owning campaign.
    pub campaign_id: Uuid,
    /// Agent that will run the probe.
    pub source_agent_id: String,
    /// Destination IP (host address, not a CIDR).
    pub destination_ip: std::net::IpAddr,
    /// Number of probes per measurement.
    pub probe_count: i16,
    /// Per-probe timeout in milliseconds.
    pub timeout_ms: i32,
    /// Inter-probe stagger in milliseconds.
    pub probe_stagger_ms: i32,
    /// When `true`, force a fresh measurement (no 24 h reuse).
    pub force_measurement: bool,
    /// Probe protocol inherited from the campaign.
    pub protocol: super::model::ProbeProtocol,
}

/// Outcome of a single [`PairDispatcher::dispatch`] call.
#[derive(Debug, Default, Clone)]
pub struct DispatchOutcome {
    /// Number of pairs successfully dispatched.
    pub dispatched: usize,
    /// IDs of pairs the dispatcher (or agent) refused. The scheduler
    /// reverts these from `dispatched` back to `pending` so they get
    /// another shot on a subsequent tick (up to `max_pair_attempts`).
    /// Without this, a rejected pair is stranded in `dispatched` —
    /// `expire_stale_attempts` only sweeps `pending` rows.
    pub rejected_ids: Vec<i64>,
    /// When the whole batch was skipped, the reason tag.
    pub skipped_reason: Option<String>,
}

/// Transport-agnostic dispatcher contract. Production impl is an
/// RPC-backed dispatcher shipped by T45; T44 uses in-memory stubs for
/// unit / integration tests.
#[async_trait]
pub trait PairDispatcher: Send + Sync {
    /// Dispatch a batch of pairs belonging to a single agent. Returns
    /// the resulting [`DispatchOutcome`].
    async fn dispatch(&self, agent_id: &str, batch: Vec<PendingPair>) -> DispatchOutcome;
}

/// In-memory dispatcher that records every call without performing any
/// side effect. Used by scheduler unit tests.
#[derive(Debug, Default, Clone)]
pub struct NoopDispatcher {
    /// History of `(agent_id, batch_size)` tuples captured in call order.
    pub calls: Arc<Mutex<Vec<(String, usize)>>>,
}

#[async_trait]
impl PairDispatcher for NoopDispatcher {
    async fn dispatch(&self, agent_id: &str, batch: Vec<PendingPair>) -> DispatchOutcome {
        let size = batch.len();
        self.calls.lock().await.push((agent_id.to_string(), size));
        DispatchOutcome {
            dispatched: size,
            ..Default::default()
        }
    }
}

/// Integration-test dispatcher that writes each pair directly to a
/// configurable terminal [`PairResolutionState`] and returns
/// successfully. Lets scheduler tests drive pairs through the state
/// machine without spinning up an agent.
pub struct DirectSettleDispatcher {
    /// Database pool used for the settlement writes.
    pub pool: PgPool,
    /// State every pair should be settled into.
    pub settle_to: PairResolutionState,
}

#[async_trait]
impl PairDispatcher for DirectSettleDispatcher {
    async fn dispatch(&self, _agent_id: &str, batch: Vec<PendingPair>) -> DispatchOutcome {
        let size = batch.len();
        for p in &batch {
            sqlx::query!(
                "UPDATE campaign_pairs
                    SET resolution_state=$2, settled_at=now()
                  WHERE id=$1",
                p.pair_id,
                self.settle_to as PairResolutionState,
            )
            .execute(&self.pool)
            .await
            .expect("DirectSettleDispatcher write");
        }
        DispatchOutcome {
            dispatched: size,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_dispatcher_records_calls() {
        let d = NoopDispatcher::default();
        let out = d
            .dispatch(
                "agent-a",
                vec![PendingPair {
                    pair_id: 1,
                    campaign_id: Uuid::nil(),
                    source_agent_id: "agent-a".into(),
                    destination_ip: "203.0.113.5".parse().unwrap(),
                    probe_count: 10,
                    timeout_ms: 2000,
                    probe_stagger_ms: 100,
                    force_measurement: false,
                    protocol: super::super::model::ProbeProtocol::Icmp,
                }],
            )
            .await;
        assert_eq!(out.dispatched, 1);
        let calls = d.calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "agent-a");
        assert_eq!(calls[0].1, 1);
    }
}
