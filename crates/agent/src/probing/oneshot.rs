//! Trippy-backed one-off prober for campaigns.
//!
//! See module docs for the per-protocol builder matrix, loss predicates,
//! MTR aggregation rules, and the shared-resource audit.

#![doc = include_str!("oneshot.md")]

use std::sync::atomic::AtomicU64;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{
    MeasurementFailure, MeasurementFailureCode, MeasurementResult, RunMeasurementBatchRequest,
};
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use tonic::Status;

use crate::command::CampaignProber;

// ---------------------------------------------------------------------------
// Collision counter (see spec 03 §6 and oneshot.md § Shared-resource audit)
// ---------------------------------------------------------------------------

/// Process-wide counter; stays at 0 in steady state. A non-zero value
/// means a trippy reply arrived whose trace identifier did not match any
/// tracer this prober spawned — a defensive guard against trace-id
/// collisions with the continuous pool. Wired by Task 7.
#[allow(dead_code)]
static ONESHOT_PROBE_COLLISIONS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Return the current collision count. Used by the coexistence test.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn oneshot_probe_collisions_total() -> u64 {
    ONESHOT_PROBE_COLLISIONS_TOTAL.load(Ordering::Relaxed)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn reset_oneshot_collisions_for_test() {
    ONESHOT_PROBE_COLLISIONS_TOTAL.store(0, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// OneshotProber
// ---------------------------------------------------------------------------

/// Trippy-backed [`CampaignProber`]. One instance per agent. Per-pair
/// blocking tracers run under an internal semaphore, independent from
/// the continuous MTR pool gated by `MESHMON_ICMP_TARGET_CONCURRENCY`.
pub struct OneshotProber {
    /// Wired by Task 3; retained here so the constructor signature stays
    /// stable while later tasks fill in real protocol aggregation.
    #[allow(dead_code)]
    semaphore: Arc<Semaphore>,
}

impl OneshotProber {
    /// Build with `max_concurrency` simultaneous tracers. 0 is treated as 1.
    pub fn new(max_concurrency: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrency.max(1))),
        }
    }
}

#[async_trait]
impl CampaignProber for OneshotProber {
    async fn run_batch(
        &self,
        req: RunMeasurementBatchRequest,
        _cancel: CancellationToken,
        results: mpsc::Sender<Result<MeasurementResult, Status>>,
    ) {
        // Skeleton: emit AgentError for every target so the transport
        // contract is honoured (one result per pair, correct pair_id).
        // Subsequent tasks replace this with real protocol aggregation.
        for target in req.targets {
            let result = MeasurementResult {
                pair_id: target.pair_id,
                outcome: Some(Outcome::Failure(MeasurementFailure {
                    code: MeasurementFailureCode::AgentError as i32,
                    detail: "oneshot prober not yet implemented".into(),
                })),
            };
            if results.send(Ok(result)).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshmon_protocol::{MeasurementKind, MeasurementTarget, Protocol};

    #[tokio::test]
    async fn skeleton_emits_one_result_per_target() {
        let prober = OneshotProber::new(4);
        let req = RunMeasurementBatchRequest {
            batch_id: 1,
            kind: MeasurementKind::Latency as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 1,
            timeout_ms: 100,
            probe_stagger_ms: 10,
            targets: (0..3)
                .map(|i| MeasurementTarget {
                    pair_id: i,
                    destination_ip: vec![127, 0, 0, 1].into(),
                    destination_port: 0,
                })
                .collect(),
        };
        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        prober.run_batch(req, cancel, tx).await;
        let mut seen: Vec<u64> = Vec::new();
        while let Some(item) = rx.recv().await {
            seen.push(item.unwrap().pair_id);
        }
        seen.sort();
        assert_eq!(seen, vec![0, 1, 2]);
    }
}
