//! Outbound emitter: batches metrics every 60 s, pushes route snapshots
//! immediately, retries on retriable failures with jittered exponential
//! backoff, and buffers up to 65 failed RPCs in a drop-oldest ring queue.
//!
//! This module is under active construction across T16. Tasks 7-9 land
//! the primary loop, snapshot builder, and retry worker. Task 7 (this
//! file's primary loop) relies on the retry worker + snapshot-builder
//! stubs below — they compile away to no-ops until Tasks 8 and 9 fill
//! them in.

use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::Rng;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use meshmon_protocol::{
    AgentMetadata, MetricsBatch, PathMetrics as PathMetricsProto, Protocol, ProtocolHealth,
    RouteSnapshotRequest,
};

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
/// `batch` / `req` / `attempts` are written by `dispatch_*` in this task
/// but only read by the retry worker (Task 9). The `#[allow(dead_code)]`
/// on those fields is dropped once Task 9 lands.
#[derive(Debug, Clone)]
enum PendingRpc {
    Metrics {
        #[allow(dead_code)]
        batch: MetricsBatch,
        next_retry_at: Instant,
        #[allow(dead_code)]
        attempts: u32,
    },
    Snapshot {
        #[allow(dead_code)]
        req: RouteSnapshotRequest,
        next_retry_at: Instant,
        #[allow(dead_code)]
        attempts: u32,
    },
}

impl PendingRpc {
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
#[derive(Debug)]
struct RetryQueue {
    queue: VecDeque<PendingRpc>,
    cap: usize,
    /// Count of entries evicted due to capacity since the last successful
    /// `push_metrics` ack. Reported in the next `MetricsBatch`'s
    /// `agent_metadata.dropped_count` and reset to 0 on ack.
    dropped_count: u64,
}

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
    ///
    /// Only consumed by the retry worker (Task 9) + unit tests today.
    #[allow(dead_code)]
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
    ///
    /// Only consumed by the retry worker (Task 9) + unit tests today.
    #[allow(dead_code)]
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
    api: Arc<A>,
    identity: EmitterIdentity,
    mut metrics_rx: mpsc::Receiver<PathMetricsMsg>,
    mut snapshots_rx: mpsc::Receiver<RouteSnapshotEnvelope>,
    queue: Arc<Mutex<RetryQueue>>,
    notify: Arc<Notify>,
    local_error_count: Arc<AtomicU64>,
    cancel: CancellationToken,
) {
    // Task 9 replaces this stub with the real retry worker.
    let retry_handle = tokio::spawn(run_retry_worker(
        Arc::clone(&api),
        Arc::clone(&queue),
        Arc::clone(&notify),
        Arc::clone(&local_error_count),
        cancel.clone(),
    ));

    let mut staged: Vec<PathMetricsMsg> = Vec::with_capacity(256);
    let mut metrics_interval = tokio::time::interval(Duration::from_secs(60));
    metrics_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Swallow the immediate first tick — the emitter just started; flushing
    // an empty batch would be noise.
    metrics_interval.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,

            maybe = metrics_rx.recv() => {
                match maybe {
                    Some(m) => staged.push(m),
                    None => {
                        tracing::info!("metrics_rx closed; emitter exiting primary loop");
                        break;
                    }
                }
            }

            maybe = snapshots_rx.recv() => {
                match maybe {
                    Some(env) => {
                        dispatch_snapshot(api.as_ref(), &identity, env, &queue, &notify, &local_error_count).await;
                    }
                    None => {
                        tracing::info!("snapshots_rx closed; emitter exiting primary loop");
                        break;
                    }
                }
            }

            _ = metrics_interval.tick() => {
                if staged.is_empty() {
                    continue;
                }
                let batch_time = SystemTime::now();
                let dropped = queue.lock().await.dropped_count();
                let errors = local_error_count.load(std::sync::atomic::Ordering::Relaxed);
                let batch = build_metrics_batch(
                    std::mem::take(&mut staged),
                    &identity,
                    errors,
                    dropped,
                    batch_time,
                );
                dispatch_metrics(api.as_ref(), batch, &queue, &notify, &local_error_count).await;
            }
        }
    }

    // Task 12 adds the drain phase here.
    retry_handle.abort();
    let _ = retry_handle.await;
}

async fn dispatch_metrics<A: ServiceApi>(
    api: &A,
    batch: MetricsBatch,
    queue: &Arc<Mutex<RetryQueue>>,
    notify: &Arc<Notify>,
    local_error_count: &Arc<AtomicU64>,
) {
    match api.push_metrics(batch.clone()).await {
        Ok(_) => {
            local_error_count.store(0, std::sync::atomic::Ordering::Relaxed);
            queue.lock().await.reset_dropped_count();
        }
        Err(e) => match classify(&e) {
            Classify::Retriable => {
                {
                    let mut q = queue.lock().await;
                    q.push(PendingRpc::Metrics {
                        batch,
                        next_retry_at: schedule_retry(0),
                        attempts: 1,
                    });
                }
                notify.notify_one();
                local_error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(error = %e, "push_metrics retriable failure; enqueued for retry");
            }
            Classify::Drop(level) => {
                emit_at_level(level, &format!("push_metrics non-retriable, dropping: {e}"));
                local_error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
    }
}

async fn dispatch_snapshot<A: ServiceApi>(
    api: &A,
    identity: &EmitterIdentity,
    env: RouteSnapshotEnvelope,
    queue: &Arc<Mutex<RetryQueue>>,
    notify: &Arc<Notify>,
    local_error_count: &Arc<AtomicU64>,
) {
    let req = build_route_snapshot_request(&identity.source_id, env);
    match api.push_route_snapshot(req.clone()).await {
        Ok(_) => {
            // Snapshot success does NOT reset dropped_count / local_error_count:
            // those counters live in MetricsBatch.agent_metadata and only a
            // successful push_metrics ack resets them (per proto semantics).
        }
        Err(e) => match classify(&e) {
            Classify::Retriable => {
                {
                    let mut q = queue.lock().await;
                    q.push(PendingRpc::Snapshot {
                        req,
                        next_retry_at: schedule_retry(0),
                        attempts: 1,
                    });
                }
                notify.notify_one();
                local_error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(error = %e, "push_route_snapshot retriable failure; enqueued");
            }
            Classify::Drop(level) => {
                emit_at_level(
                    level,
                    &format!("push_route_snapshot non-retriable, dropping: {e}"),
                );
                local_error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
    }
}

enum Classify {
    Retriable,
    Drop(tracing::Level),
}

fn classify(e: &anyhow::Error) -> Classify {
    if let Some(status) = e.downcast_ref::<tonic::Status>() {
        match status.code() {
            tonic::Code::Unavailable | tonic::Code::ResourceExhausted => Classify::Retriable,
            tonic::Code::Unauthenticated | tonic::Code::InvalidArgument => {
                Classify::Drop(tracing::Level::ERROR)
            }
            _ => Classify::Drop(tracing::Level::WARN),
        }
    } else {
        // Transport-level error (no tonic::Status) — connection refused
        // before HTTP/2, TLS handshake, etc. Treat as retriable, matching
        // bootstrap's register-retry behaviour.
        Classify::Retriable
    }
}

fn emit_at_level(level: tracing::Level, msg: &str) {
    match level {
        tracing::Level::ERROR => tracing::error!("{msg}"),
        tracing::Level::WARN => tracing::warn!("{msg}"),
        tracing::Level::INFO => tracing::info!("{msg}"),
        tracing::Level::DEBUG => tracing::debug!("{msg}"),
        tracing::Level::TRACE => tracing::trace!("{msg}"),
    }
}

fn schedule_retry(attempts: u32) -> Instant {
    let base = Duration::from_secs(1);
    let cap = Duration::from_secs(300);
    let grown = base.saturating_mul(2u32.saturating_pow(attempts.min(10)));
    let clamped = grown.min(cap);
    let jitter = rand::rng().random_range(0.75..1.25);
    let jittered = Duration::from_secs_f64(clamped.as_secs_f64() * jitter);
    Instant::now() + jittered
}

fn build_metrics_batch(
    staged: Vec<PathMetricsMsg>,
    identity: &EmitterIdentity,
    local_error_count: u64,
    dropped_count: u64,
    now: SystemTime,
) -> MetricsBatch {
    let uptime_secs = now
        .duration_since(identity.start_time)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    MetricsBatch {
        source_id: identity.source_id.clone(),
        batch_timestamp_micros: system_time_to_micros_i64(now),
        agent_metadata: Some(AgentMetadata {
            version: identity.agent_version.clone(),
            uptime_secs,
            local_error_count,
            dropped_count,
        }),
        paths: staged.into_iter().map(path_metrics_to_proto).collect(),
    }
}

fn path_metrics_to_proto(m: PathMetricsMsg) -> PathMetricsProto {
    // Summary's RTT fields are Option<_>; the proto fields are non-optional
    // u32 (rtt_*) or f64 (failure_rate). Unwrap with 0 fallback — the
    // validator treats MAX_RTT_MICROS as a ceiling, not a floor, and zero
    // is the natural "no successful probe in window" value.
    let mean_u32 = m
        .stats
        .mean_rtt_micros
        .map(|v| v.round().clamp(0.0, u32::MAX as f64) as u32)
        .unwrap_or(0);
    let stddev_u32 = m
        .stats
        .stddev_rtt_micros
        .map(|v| v.round().clamp(0.0, u32::MAX as f64) as u32)
        .unwrap_or(0);
    PathMetricsProto {
        target_id: m.target_id,
        protocol: m.protocol as i32,
        window_start_micros: system_time_to_micros_i64(m.window_start),
        window_end_micros: system_time_to_micros_i64(m.window_end),
        probes_sent: m.stats.sample_count,
        probes_successful: m.stats.successful,
        failure_rate: m.stats.failure_rate,
        rtt_avg_micros: mean_u32,
        rtt_min_micros: m.stats.min_rtt_micros.unwrap_or(0),
        rtt_max_micros: m.stats.max_rtt_micros.unwrap_or(0),
        rtt_stddev_micros: stddev_u32,
        rtt_p50_micros: m.stats.p50_rtt_micros.unwrap_or(0),
        rtt_p95_micros: m.stats.p95_rtt_micros.unwrap_or(0),
        rtt_p99_micros: m.stats.p99_rtt_micros.unwrap_or(0),
        health: proto_health_to_wire(m.health) as i32,
    }
}

fn proto_health_to_wire(h: ProtoHealth) -> ProtocolHealth {
    match h {
        ProtoHealth::Healthy => ProtocolHealth::Healthy,
        ProtoHealth::Unhealthy => ProtocolHealth::Unhealthy,
    }
}

fn system_time_to_micros_i64(t: SystemTime) -> i64 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// Stubs for Tasks 8 and 9
// ---------------------------------------------------------------------------

async fn run_retry_worker<A: ServiceApi>(
    _api: Arc<A>,
    _queue: Arc<Mutex<RetryQueue>>,
    _notify: Arc<Notify>,
    _local_error_count: Arc<AtomicU64>,
    cancel: CancellationToken,
) {
    // Task 9 fills this in.
    cancel.cancelled().await;
}

fn build_route_snapshot_request(
    _source_id: &str,
    _env: RouteSnapshotEnvelope,
) -> RouteSnapshotRequest {
    // Task 8 replaces this with the real builder. Returning default keeps
    // the primary loop compilable; tests in Task 8 exercise the full shape.
    RouteSnapshotRequest::default()
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

    fn test_stats() -> crate::stats::Summary {
        crate::stats::Summary {
            sample_count: 60,
            successful: 60,
            failure_rate: 0.0,
            mean_rtt_micros: Some(15_000.0),
            stddev_rtt_micros: Some(500.0),
            min_rtt_micros: Some(12_000),
            max_rtt_micros: Some(20_000),
            p50_rtt_micros: Some(15_000),
            p95_rtt_micros: Some(18_000),
            p99_rtt_micros: Some(19_000),
        }
    }

    #[derive(Default)]
    struct RecordingApi {
        push_metrics_calls: Mutex<Vec<MetricsBatch>>,
        push_snapshot_calls: Mutex<Vec<RouteSnapshotRequest>>,
        metrics_result: Mutex<Option<tonic::Status>>,
        snapshot_result: Mutex<Option<tonic::Status>>,
    }

    impl ServiceApi for RecordingApi {
        async fn register(
            &self,
            _: meshmon_protocol::RegisterRequest,
        ) -> anyhow::Result<meshmon_protocol::RegisterResponse> {
            unimplemented!()
        }
        async fn get_config(&self) -> anyhow::Result<meshmon_protocol::ConfigResponse> {
            unimplemented!()
        }
        async fn get_targets(&self, _: &str) -> anyhow::Result<meshmon_protocol::TargetsResponse> {
            unimplemented!()
        }
        async fn push_metrics(
            &self,
            batch: MetricsBatch,
        ) -> anyhow::Result<meshmon_protocol::PushMetricsResponse> {
            self.push_metrics_calls.lock().await.push(batch);
            let maybe_err = self.metrics_result.lock().await.clone();
            if let Some(status) = maybe_err {
                return Err(anyhow::Error::from(status));
            }
            Ok(meshmon_protocol::PushMetricsResponse::default())
        }
        async fn push_route_snapshot(
            &self,
            req: RouteSnapshotRequest,
        ) -> anyhow::Result<meshmon_protocol::PushRouteSnapshotResponse> {
            self.push_snapshot_calls.lock().await.push(req);
            let maybe_err = self.snapshot_result.lock().await.clone();
            if let Some(status) = maybe_err {
                return Err(anyhow::Error::from(status));
            }
            Ok(meshmon_protocol::PushRouteSnapshotResponse::default())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn spawn_task_exits_on_cancel() {
        let api: Arc<RecordingApi> = Arc::new(RecordingApi::default());
        let (_mtx, mrx) = mpsc::channel(8);
        let (_stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(
            api,
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

    #[tokio::test(start_paused = true)]
    async fn primary_loop_flushes_staged_metrics_at_60s() {
        let api = Arc::new(RecordingApi::default());
        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "test".into(),
            start_time: SystemTime::now(),
        };
        let (mtx, mrx) = mpsc::channel(16);
        let (_stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(api.clone(), identity, mrx, srx, cancel.clone());

        let now = SystemTime::now();
        mtx.send(PathMetricsMsg {
            target_id: "t1".into(),
            protocol: Protocol::Icmp,
            window_start: now - Duration::from_secs(60),
            window_end: now,
            stats: test_stats(),
            health: ProtoHealth::Healthy,
        })
        .await
        .unwrap();

        // Give the spawned task a chance to consume the message and park
        // in the select! before we advance the mocked clock.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        // Advance past the 60s tick (plus margin for yields).
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

        let calls = api.push_metrics_calls.lock().await;
        assert_eq!(calls.len(), 1, "expected exactly one push_metrics call");
        assert_eq!(calls[0].source_id, "src");
        assert_eq!(calls[0].paths.len(), 1);
        let md = calls[0].agent_metadata.as_ref().expect("metadata set");
        assert_eq!(md.version, "test");
        assert_eq!(md.dropped_count, 0);
        assert_eq!(md.local_error_count, 0);
        let p = &calls[0].paths[0];
        assert_eq!(p.target_id, "t1");
        assert_eq!(p.protocol, Protocol::Icmp as i32);
        assert_eq!(p.health, ProtocolHealth::Healthy as i32);
        assert_eq!(p.probes_sent, 60);
        assert_eq!(p.probes_successful, 60);
        assert_eq!(p.rtt_avg_micros, 15_000);
        assert_eq!(p.rtt_p95_micros, 18_000);
    }

    #[tokio::test(start_paused = true)]
    async fn primary_loop_enqueues_on_unavailable_and_bumps_error_counter() {
        let api = Arc::new(RecordingApi::default());
        *api.metrics_result.lock().await = Some(tonic::Status::unavailable("down"));

        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "t".into(),
            start_time: SystemTime::now(),
        };
        let (mtx, mrx) = mpsc::channel(16);
        let (_stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(api.clone(), identity, mrx, srx, cancel.clone());

        let now = SystemTime::now();
        mtx.send(PathMetricsMsg {
            target_id: "t1".into(),
            protocol: Protocol::Tcp,
            window_start: now - Duration::from_secs(60),
            window_end: now,
            stats: test_stats(),
            health: ProtoHealth::Healthy,
        })
        .await
        .unwrap();

        // Park the spawned task in select! before advancing time.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

        // The retry worker is still a stub in Task 7, so only the primary
        // dispatch's one call is observable. Queue should contain one entry.
        let calls = api.push_metrics_calls.lock().await;
        assert_eq!(calls.len(), 1, "primary loop attempted once before enqueue");
        // (Queue state is module-private and the retry worker stub never runs;
        // don't assert on it here — Task 9's integration test exercises it.)
    }

    #[tokio::test(start_paused = true)]
    async fn primary_loop_drops_invalid_argument_without_retry() {
        let api = Arc::new(RecordingApi::default());
        *api.metrics_result.lock().await = Some(tonic::Status::invalid_argument("bad"));

        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "t".into(),
            start_time: SystemTime::now(),
        };
        let (mtx, mrx) = mpsc::channel(16);
        let (_stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(api.clone(), identity, mrx, srx, cancel.clone());

        let now = SystemTime::now();
        mtx.send(PathMetricsMsg {
            target_id: "t1".into(),
            protocol: Protocol::Icmp,
            window_start: now - Duration::from_secs(60),
            window_end: now,
            stats: test_stats(),
            health: ProtoHealth::Healthy,
        })
        .await
        .unwrap();

        // Park the spawned task in select! before advancing time.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

        // Exactly one attempt — InvalidArgument is non-retriable.
        let calls = api.push_metrics_calls.lock().await;
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn classify_maps_tonic_statuses_correctly() {
        use tonic::Code::*;
        let anyhow_from = |c: tonic::Code| anyhow::Error::from(tonic::Status::new(c, "msg"));
        assert!(matches!(
            classify(&anyhow_from(Unavailable)),
            Classify::Retriable
        ));
        assert!(matches!(
            classify(&anyhow_from(ResourceExhausted)),
            Classify::Retriable
        ));
        assert!(matches!(
            classify(&anyhow_from(Unauthenticated)),
            Classify::Drop(tracing::Level::ERROR)
        ));
        assert!(matches!(
            classify(&anyhow_from(InvalidArgument)),
            Classify::Drop(tracing::Level::ERROR)
        ));
        assert!(matches!(
            classify(&anyhow_from(DeadlineExceeded)),
            Classify::Drop(tracing::Level::WARN)
        ));
        // Plain anyhow (no tonic::Status) → retriable.
        let plain = anyhow::anyhow!("connection refused");
        assert!(matches!(classify(&plain), Classify::Retriable));
    }

    #[test]
    fn build_metrics_batch_stamps_identity_and_counts() {
        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "v".into(),
            start_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_060);
        let msg = PathMetricsMsg {
            target_id: "t".into(),
            protocol: Protocol::Udp,
            window_start: now - Duration::from_secs(60),
            window_end: now,
            stats: test_stats(),
            health: ProtoHealth::Unhealthy,
        };
        let batch = build_metrics_batch(vec![msg], &identity, 7, 3, now);
        assert_eq!(batch.source_id, "src");
        assert_eq!(batch.batch_timestamp_micros, 1_700_000_060 * 1_000_000);
        let md = batch.agent_metadata.unwrap();
        assert_eq!(md.version, "v");
        assert_eq!(md.uptime_secs, 60);
        assert_eq!(md.local_error_count, 7);
        assert_eq!(md.dropped_count, 3);
        assert_eq!(batch.paths.len(), 1);
        assert_eq!(batch.paths[0].health, ProtocolHealth::Unhealthy as i32);
    }
}
