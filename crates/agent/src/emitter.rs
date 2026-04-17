//! Outbound emitter: batches metrics every 60 s, pushes route snapshots
//! immediately, retries on retriable failures with jittered exponential
//! backoff, and buffers up to 65 failed RPCs in a drop-oldest ring queue.
//!
//! This module is under active construction across T16. Only the
//! supervisor → emitter message type is stable here; the runtime + retry
//! worker land in subsequent tasks.

use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use meshmon_protocol::{MetricsBatch, Protocol, RouteSnapshotRequest};

use crate::api::ServiceApi;
use crate::route::RouteSnapshotEnvelope;
use crate::state::ProtoHealth;
use crate::stats::Summary;

/// One per-(target, protocol) metrics record produced by the supervisor's
/// 60 s metrics tick. The emitter batches these into a `MetricsBatch`.
///
/// `health` is a concrete `ProtoHealth` (never `Unspecified`): the
/// supervisor drops the emission when its last-evaluated
/// `TargetSnapshot` still carries `None` for that protocol, which
/// happens only before the first eval tick has fired. After the first
/// eval tick, the state machine always classifies every protocol as
/// `Healthy` or `Unhealthy`, so this struct carries a real verdict.
/// The service rejects `ProtocolHealth::Unspecified` with
/// `INVALID_ARGUMENT`, so this invariant is load-bearing for wire
/// validity.
#[derive(Debug, Clone)]
pub struct PathMetricsMsg {
    pub target_id: String,
    pub protocol: Protocol,
    pub window_start: SystemTime,
    pub window_end: SystemTime,
    pub stats: Summary,
    pub health: ProtoHealth,
}

/// A failed RPC awaiting retry. One `VecDeque<PendingRpc>` is shared
/// between the primary loop (producer) and the retry worker (consumer).
///
/// Constructed and consumed only by Tasks 7-10; the lib-level build sees
/// no callers yet, so `dead_code` is silenced until the primary loop
/// starts enqueueing failures.
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum PendingRpc {
    Metrics {
        batch: MetricsBatch,
        next_retry_at: Instant,
        attempts: u32,
    },
    Snapshot {
        req: RouteSnapshotRequest,
        next_retry_at: Instant,
        attempts: u32,
    },
}

impl PendingRpc {
    #[allow(dead_code)]
    fn next_retry_at(&self) -> Instant {
        match self {
            Self::Metrics { next_retry_at, .. } | Self::Snapshot { next_retry_at, .. } => {
                *next_retry_at
            }
        }
    }
}

/// Bounded drop-oldest queue of pending RPCs.
///
/// The queue holds at most `cap` entries. Pushing into a full queue evicts
/// the oldest entry and increments `dropped_count`. Capacity sizing (see
/// Spec 02 §Emitter) is 65 entries ≈ ~230 KiB RAM ceiling — enough for an
/// hour of retries at normal cadence.
///
/// The struct + its methods are populated in Tasks 9-10; until then only
/// the test module exercises them, so `dead_code` is silenced on the
/// lib-side view.
#[derive(Debug)]
#[allow(dead_code)]
struct RetryQueue {
    queue: VecDeque<PendingRpc>,
    cap: usize,
    /// Count of entries evicted due to capacity since the last successful
    /// `push_metrics` ack. Reported in the next `MetricsBatch`'s
    /// `agent_metadata.dropped_count` and reset to 0 on ack.
    dropped_count: u64,
}

#[allow(dead_code)]
impl RetryQueue {
    fn new(cap: usize) -> Self {
        assert!(cap > 0, "RetryQueue::new requires cap > 0");
        Self {
            queue: VecDeque::with_capacity(cap),
            cap,
            dropped_count: 0,
        }
    }

    /// Push a pending RPC, evicting the oldest on overflow and bumping
    /// `dropped_count`. Returns `true` iff an eviction happened.
    fn push(&mut self, item: PendingRpc) -> bool {
        let dropped = if self.queue.len() >= self.cap {
            self.queue.pop_front();
            self.dropped_count = self.dropped_count.saturating_add(1);
            true
        } else {
            false
        };
        self.queue.push_back(item);
        dropped
    }

    /// Take up to `n` items whose `next_retry_at <= now`, preserving the
    /// relative order of the remaining (not-yet-due) entries.
    fn take_due(&mut self, now: Instant, n: usize) -> Vec<PendingRpc> {
        let mut out = Vec::new();
        let mut remaining = VecDeque::with_capacity(self.queue.len());
        for item in self.queue.drain(..) {
            if out.len() < n && item.next_retry_at() <= now {
                out.push(item);
            } else {
                remaining.push_back(item);
            }
        }
        self.queue = remaining;
        out
    }

    /// Minimum `next_retry_at` across the queue, or `None` if empty.
    fn next_due(&self) -> Option<Instant> {
        self.queue.iter().map(|p| p.next_retry_at()).min()
    }

    fn reset_dropped_count(&mut self) {
        self.dropped_count = 0;
    }

    fn dropped_count(&self) -> u64 {
        self.dropped_count
    }
}

/// Agent identity + metadata every outbound envelope needs. Built once
/// at bootstrap time and handed to the emitter at `spawn`.
#[derive(Debug, Clone)]
pub struct EmitterIdentity {
    pub source_id: String,
    pub agent_version: String,
    pub start_time: SystemTime,
}

/// Ring-buffer capacity per Spec 02 §Emitter. 65 entries ≈ ~230 KiB RAM.
const RETRY_QUEUE_CAP: usize = 65;

/// Spawn the emitter task and return its `JoinHandle` so
/// [`AgentRuntime::shutdown`](crate::bootstrap::AgentRuntime) can await
/// a clean drain.
pub fn spawn<A: ServiceApi>(
    api: Arc<A>,
    identity: EmitterIdentity,
    metrics_rx: mpsc::Receiver<PathMetricsMsg>,
    snapshots_rx: mpsc::Receiver<RouteSnapshotEnvelope>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    let queue = Arc::new(Mutex::new(RetryQueue::new(RETRY_QUEUE_CAP)));
    let notify = Arc::new(Notify::new());
    let local_error_count = Arc::new(AtomicU64::new(0));

    tokio::spawn(run_emitter(
        api,
        identity,
        metrics_rx,
        snapshots_rx,
        queue,
        notify,
        local_error_count,
        cancel,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_emitter<A: ServiceApi>(
    _api: Arc<A>,
    _identity: EmitterIdentity,
    _metrics_rx: mpsc::Receiver<PathMetricsMsg>,
    _snapshots_rx: mpsc::Receiver<RouteSnapshotEnvelope>,
    _queue: Arc<Mutex<RetryQueue>>,
    _notify: Arc<Notify>,
    _local_error_count: Arc<AtomicU64>,
    cancel: CancellationToken,
) {
    // Tasks 7-10 fill this in. For now the task just waits for cancel so
    // the public API is usable (tests can spawn + cancel without a stub
    // loop body).
    cancel.cancelled().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn mk_snapshot_pending(now: Instant, offset_ms: u64) -> PendingRpc {
        PendingRpc::Snapshot {
            req: RouteSnapshotRequest::default(),
            next_retry_at: now + Duration::from_millis(offset_ms),
            attempts: 0,
        }
    }

    fn mk_metrics_pending(now: Instant, offset_ms: u64) -> PendingRpc {
        PendingRpc::Metrics {
            batch: MetricsBatch::default(),
            next_retry_at: now + Duration::from_millis(offset_ms),
            attempts: 0,
        }
    }

    #[test]
    fn retry_queue_drop_oldest_on_full() {
        let now = Instant::now();
        let mut q = RetryQueue::new(2);
        assert!(!q.push(mk_snapshot_pending(now, 0))); // oldest, will be evicted
        assert!(!q.push(mk_snapshot_pending(now, 100)));
        assert!(q.push(mk_snapshot_pending(now, 200))); // evicts offset=0
        assert_eq!(q.queue.len(), 2);
        assert_eq!(q.dropped_count(), 1);

        // Verify the specific entry evicted was the oldest (offset=0) and the
        // two survivors appear in original insertion order.
        let due = q.take_due(now + Duration::from_millis(500), 10);
        let retries: Vec<_> = due.iter().map(|p| p.next_retry_at()).collect();
        assert_eq!(
            retries,
            vec![
                now + Duration::from_millis(100),
                now + Duration::from_millis(200),
            ]
        );
    }

    #[test]
    fn retry_queue_take_due_respects_timestamp_order() {
        let now = Instant::now();
        let mut q = RetryQueue::new(10);
        q.push(mk_snapshot_pending(now, 1000));
        q.push(mk_snapshot_pending(now, 0)); // already due
        q.push(mk_snapshot_pending(now, 500));
        let due = q.take_due(now + Duration::from_millis(10), 10);
        assert_eq!(due.len(), 1);
        assert_eq!(q.queue.len(), 2);
    }

    #[test]
    fn retry_queue_take_due_caps_at_n() {
        let now = Instant::now();
        let mut q = RetryQueue::new(10);
        // Offsets 0..=4 — all due at now+10ms; n=3 caps the drain.
        for i in 0..5u64 {
            q.push(mk_snapshot_pending(now, i));
        }
        let due = q.take_due(now + Duration::from_millis(10), 3);
        assert_eq!(due.len(), 3);
        // Survivors preserve the original FIFO order: offsets 3, 4.
        assert_eq!(q.queue.len(), 2);
        let survivor_offsets: Vec<_> = q.queue.iter().map(|p| p.next_retry_at()).collect();
        assert_eq!(
            survivor_offsets,
            vec![
                now + Duration::from_millis(3),
                now + Duration::from_millis(4),
            ]
        );
        // And the first three popped are the originally-oldest three (0, 1, 2).
        let popped: Vec<_> = due.iter().map(|p| p.next_retry_at()).collect();
        assert_eq!(
            popped,
            vec![
                now + Duration::from_millis(0),
                now + Duration::from_millis(1),
                now + Duration::from_millis(2),
            ]
        );
    }

    #[test]
    #[should_panic(expected = "RetryQueue::new requires cap > 0")]
    fn retry_queue_new_zero_panics() {
        let _ = RetryQueue::new(0);
    }

    #[test]
    fn retry_queue_reset_dropped_count() {
        let now = Instant::now();
        let mut q = RetryQueue::new(1);
        q.push(mk_snapshot_pending(now, 0));
        q.push(mk_snapshot_pending(now, 100)); // drop
        assert_eq!(q.dropped_count(), 1);
        q.reset_dropped_count();
        assert_eq!(q.dropped_count(), 0);
    }

    #[test]
    fn retry_queue_next_due_returns_min() {
        let now = Instant::now();
        let mut q = RetryQueue::new(10);
        assert_eq!(q.next_due(), None);
        q.push(mk_snapshot_pending(now, 500));
        q.push(mk_metrics_pending(now, 100));
        q.push(mk_snapshot_pending(now, 300));
        let nd = q.next_due().expect("queue non-empty");
        assert_eq!(nd, now + Duration::from_millis(100));
    }

    #[test]
    fn retry_queue_pending_rpc_variant_exposes_next_retry_at() {
        let now = Instant::now();
        let metrics = mk_metrics_pending(now, 42);
        assert_eq!(metrics.next_retry_at(), now + Duration::from_millis(42));
        let snap = mk_snapshot_pending(now, 99);
        assert_eq!(snap.next_retry_at(), now + Duration::from_millis(99));
    }

    #[tokio::test(start_paused = true)]
    async fn spawn_task_exits_on_cancel() {
        // Minimal ServiceApi stub; none of its methods get called by the
        // Task-6 stub body.
        struct Nop;
        impl ServiceApi for Nop {
            async fn register(
                &self,
                _: meshmon_protocol::RegisterRequest,
            ) -> anyhow::Result<meshmon_protocol::RegisterResponse> {
                unimplemented!()
            }
            async fn get_config(&self) -> anyhow::Result<meshmon_protocol::ConfigResponse> {
                unimplemented!()
            }
            async fn get_targets(
                &self,
                _: &str,
            ) -> anyhow::Result<meshmon_protocol::TargetsResponse> {
                unimplemented!()
            }
            async fn push_metrics(
                &self,
                _: MetricsBatch,
            ) -> anyhow::Result<meshmon_protocol::PushMetricsResponse> {
                unimplemented!()
            }
            async fn push_route_snapshot(
                &self,
                _: RouteSnapshotRequest,
            ) -> anyhow::Result<meshmon_protocol::PushRouteSnapshotResponse> {
                unimplemented!()
            }
        }

        let (_mtx, mrx) = mpsc::channel(8);
        let (_stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(
            Arc::new(Nop),
            EmitterIdentity {
                source_id: "src".into(),
                agent_version: "t".into(),
                start_time: SystemTime::now(),
            },
            mrx,
            srx,
            cancel.clone(),
        );
        // Give the task one poll then cancel.
        tokio::task::yield_now().await;
        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(res.is_ok(), "emitter task should exit within 1s of cancel");
    }
}
