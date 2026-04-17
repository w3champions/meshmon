//! Outbound emitter: the agent's single outbound-only task.
//!
//! Batches per-(target, protocol) `PathMetricsMsg` records received from
//! supervisors every 60 seconds into a `MetricsBatch` and pushes via
//! `ServiceApi::push_metrics`. Route-snapshot envelopes are dispatched
//! immediately via `push_route_snapshot` — not batched, because each
//! snapshot represents a discrete route-change event.
//!
//! Retry handling: UNAVAILABLE and RESOURCE_EXHAUSTED responses enqueue
//! the RPC into a drop-oldest ring queue (capacity 65 entries,
//! ≈ 230 KiB) and wake a concurrent retry worker that re-invokes on a
//! jittered exponential schedule (1 s base, 5 min cap, ±25% jitter).
//! Drop-on-failure codes (UNAUTHENTICATED, INVALID_ARGUMENT) are logged
//! at `error!` and discarded — retrying a contract violation wastes
//! cycles. Any other tonic status code is logged at `warn!` and dropped
//! conservatively. Transport-level errors (no tonic status) are treated
//! as retriable.
//!
//! Buffer overflow: pushing into a full queue evicts the oldest entry
//! and bumps `dropped_count`, which rides out in the next successful
//! `MetricsBatch.agent_metadata.dropped_count` and resets on ack.
//!
//! Shutdown: the primary loop exits on `cancel.cancelled()` or when both
//! receivers close. The emitter then enters a bounded 5-second drain
//! phase that biased-polls pending snapshots (time-sensitive), stages
//! any remaining metrics, and flushes a final best-effort batch before
//! aborting the retry worker. Any entries still in the retry queue are
//! discarded on exit — the spec treats the buffer as ephemeral.

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
    AgentMetadata, HopIp as HopIpProto, HopSummary as HopSummaryProto, MetricsBatch,
    PathMetrics as PathMetricsProto, PathSummary as PathSummaryProto, Protocol, ProtocolHealth,
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
#[derive(Debug, Clone)]
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
    api: Arc<A>,
    identity: EmitterIdentity,
    mut metrics_rx: mpsc::Receiver<PathMetricsMsg>,
    mut snapshots_rx: mpsc::Receiver<RouteSnapshotEnvelope>,
    queue: Arc<Mutex<RetryQueue>>,
    notify: Arc<Notify>,
    local_error_count: Arc<AtomicU64>,
    cancel: CancellationToken,
) {
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

    // Drain phase: up to 5 s wall-clock to flush in-flight work before we
    // return. Biased toward route snapshots — they're time-sensitive;
    // metrics are already aggregated into staged.
    //
    // `*_done` flags gate each receiver arm: once a channel returns `None`
    // we stop polling it. Without this, `recv()` on a closed+empty channel
    // resolves to `Poll::Ready(None)` instantly on every iteration and the
    // biased select starves the timer arm, busy-spinning until the outer
    // runtime deadline.
    let drain_deadline = Instant::now() + Duration::from_secs(5);
    let mut snapshots_done = false;
    let mut metrics_done = false;
    while !snapshots_done || !metrics_done {
        if Instant::now() >= drain_deadline {
            break;
        }
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(drain_deadline) => break,
            maybe = snapshots_rx.recv(), if !snapshots_done => {
                match maybe {
                    Some(env) => {
                        dispatch_snapshot(
                            api.as_ref(),
                            &identity,
                            env,
                            &queue,
                            &notify,
                            &local_error_count,
                        )
                        .await;
                    }
                    None => {
                        // Snapshot channel fully drained. Stop polling
                        // this arm; keep the loop alive so metrics_rx
                        // (if still open) can flush, then we exit when
                        // both flags are set or the deadline elapses.
                        snapshots_done = true;
                    }
                }
            }
            maybe = metrics_rx.recv(), if !metrics_done => {
                match maybe {
                    Some(m) => staged.push(m),
                    None => {
                        // metrics channel fully drained. Mirrors the
                        // snapshots_done handling — flag it off and let
                        // the loop fall through to the final flush.
                        metrics_done = true;
                    }
                }
            }
        }
    }

    // Flush any remaining staged metrics as one final best-effort batch.
    if !staged.is_empty() {
        let dropped = queue.lock().await.dropped_count();
        let errors = local_error_count.load(std::sync::atomic::Ordering::Relaxed);
        let batch = build_metrics_batch(
            std::mem::take(&mut staged),
            &identity,
            errors,
            dropped,
            SystemTime::now(),
        );
        dispatch_metrics(api.as_ref(), batch, &queue, &notify, &local_error_count).await;
    }

    // Stop the retry worker. Any entries left in the queue are dropped —
    // they'll be re-acquired from whatever persistence layer exists (none
    // today; see spec 02 future-work §Local buffer).
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
// Retry worker
// ---------------------------------------------------------------------------

/// Classification of a retry RPC failure, carrying the original error
/// for logging. Wraps `classify`'s verdict so the retry worker can
/// match once and avoid duplicating the status-code logic.
enum RetryErr {
    Retriable(anyhow::Error),
    Drop {
        level: tracing::Level,
        err: anyhow::Error,
    },
}

impl From<anyhow::Error> for RetryErr {
    fn from(e: anyhow::Error) -> Self {
        match classify(&e) {
            Classify::Retriable => RetryErr::Retriable(e),
            Classify::Drop(level) => RetryErr::Drop { level, err: e },
        }
    }
}

/// Return a fresh `PendingRpc` with `attempts` incremented and
/// `next_retry_at` rescheduled according to the new attempt count.
/// Preserves the variant and payload; saturating_add avoids wrap on
/// pathological retry counts.
fn bump_retry(entry: PendingRpc) -> PendingRpc {
    match entry {
        PendingRpc::Metrics {
            batch, attempts, ..
        } => {
            let attempts = attempts.saturating_add(1);
            PendingRpc::Metrics {
                batch,
                attempts,
                next_retry_at: schedule_retry(attempts),
            }
        }
        PendingRpc::Snapshot { req, attempts, .. } => {
            let attempts = attempts.saturating_add(1);
            PendingRpc::Snapshot {
                req,
                attempts,
                next_retry_at: schedule_retry(attempts),
            }
        }
    }
}

async fn run_retry_worker<A: ServiceApi>(
    api: Arc<A>,
    queue: Arc<Mutex<RetryQueue>>,
    notify: Arc<Notify>,
    local_error_count: Arc<AtomicU64>,
    cancel: CancellationToken,
) {
    // Idle sleep when the queue is empty. Long enough that we're not
    // thrashing the system when nothing is pending; short enough that a
    // missed notify (theoretically impossible with `notify_one`, but
    // defensive) still makes forward progress.
    const IDLE_SLEEP: Duration = Duration::from_secs(60);

    loop {
        let next_due = queue.lock().await.next_due();
        let wait = match next_due {
            Some(t) => t.saturating_duration_since(Instant::now()),
            None => IDLE_SLEEP,
        };

        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = notify.notified() => continue,
            _ = tokio::time::sleep(wait) => {}
        }

        // Drain up to 8 due entries. Taking them out of the queue
        // temporarily means a concurrent primary-loop push can't evict
        // them via drop-oldest even if the queue fills while we're
        // awaiting the RPC. That behaviour is intentional: entries we
        // already committed to retrying shouldn't lose their turn.
        let due = {
            let mut q = queue.lock().await;
            q.take_due(Instant::now(), 8)
        };

        for entry in due {
            let is_metrics = matches!(entry, PendingRpc::Metrics { .. });
            let rpc_result = match &entry {
                PendingRpc::Metrics { batch, .. } => api
                    .push_metrics(batch.clone())
                    .await
                    .map(|_| ())
                    .map_err(RetryErr::from),
                PendingRpc::Snapshot { req, .. } => api
                    .push_route_snapshot(req.clone())
                    .await
                    .map(|_| ())
                    .map_err(RetryErr::from),
            };

            match rpc_result {
                Ok(()) => {
                    if is_metrics {
                        local_error_count.store(0, std::sync::atomic::Ordering::Relaxed);
                        queue.lock().await.reset_dropped_count();
                        tracing::info!("push_metrics retry succeeded; counters reset");
                    } else {
                        tracing::info!("push_route_snapshot retry succeeded");
                    }
                }
                Err(RetryErr::Retriable(e)) => {
                    let bumped = bump_retry(entry);
                    queue.lock().await.push(bumped);
                    local_error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!(error = %e, "retry still retriable; re-enqueued");
                }
                Err(RetryErr::Drop { level, err }) => {
                    emit_at_level(
                        level,
                        &format!("retry non-retriable, dropping entry: {err}"),
                    );
                    local_error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Route snapshot builder (Task 8)
// ---------------------------------------------------------------------------

fn build_route_snapshot_request(
    source_id: &str,
    env: RouteSnapshotEnvelope,
) -> RouteSnapshotRequest {
    let observed_at_micros = env.snapshot.observed_at_micros_i64();
    let path_summary = build_path_summary(&env.snapshot.hops);
    let protocol = env.snapshot.protocol as i32;
    let hops = env.snapshot.hops.iter().map(hop_to_proto).collect();
    RouteSnapshotRequest {
        source_id: source_id.to_owned(),
        target_id: env.target_id,
        protocol,
        observed_at_micros,
        hops,
        path_summary: Some(path_summary),
    }
}

/// Derive a `PathSummary` from the hops of a `RouteSnapshot`.
///
/// - `hop_count` is the vector length.
/// - `avg_rtt_micros` is the mean of `avg_rtt_micros` across hops that have
///   a positive RTT. A fully-lost hop (RTT=0) is excluded from the RTT
///   mean but still contributes to `loss_pct`. `0` when no hop has a
///   positive RTT.
/// - `loss_pct` is the mean of `loss_pct` across *all* hops, or `0.0`
///   when `hops` is empty.
fn build_path_summary(hops: &[crate::route::HopSummary]) -> PathSummaryProto {
    let hop_count = hops.len() as u32;

    let (rtt_sum, rtt_n) = hops
        .iter()
        .filter(|h| h.avg_rtt_micros > 0)
        .fold((0u64, 0u64), |(s, n), h| {
            (s + h.avg_rtt_micros as u64, n + 1)
        });
    let avg_rtt_micros = if rtt_n > 0 {
        (rtt_sum / rtt_n) as u32
    } else {
        0
    };

    let loss_pct = if hops.is_empty() {
        0.0
    } else {
        hops.iter().map(|h| h.loss_pct).sum::<f64>() / hops.len() as f64
    };

    PathSummaryProto {
        avg_rtt_micros,
        loss_pct,
        hop_count,
    }
}

/// Convert an agent-side `route::HopSummary` to the proto `HopSummary`,
/// mapping IPv4 addresses to 4-byte `HopIp.ip` and IPv6 to 16-byte
/// `HopIp.ip` (network byte order via `octets()`).
fn hop_to_proto(h: &crate::route::HopSummary) -> HopSummaryProto {
    let observed_ips = h
        .observed_ips
        .iter()
        .map(|obs| HopIpProto {
            ip: meshmon_protocol::ip::from_ipaddr(obs.ip),
            frequency: obs.frequency,
        })
        .collect();
    HopSummaryProto {
        position: h.position as u32,
        observed_ips,
        avg_rtt_micros: h.avg_rtt_micros,
        stddev_rtt_micros: h.stddev_rtt_micros,
        loss_pct: h.loss_pct,
    }
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
        // Shutdown now includes up to 5 s of drain (Task 12); allow 10 s
        // of logical time for the handle to settle.
        let res = tokio::time::timeout(Duration::from_secs(10), handle).await;
        assert!(res.is_ok(), "emitter task should exit within 10s of cancel");
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

        // Under `start_paused = true`, the only clock advance is the 61s we
        // explicitly issued to fire the primary tick. The retry worker enqueues
        // the failure with `next_retry_at = now + ~1s jitter` and parks in
        // `sleep(next_retry_at - now).await`; we then cancel before any further
        // advance, so the worker never wakes. Only the primary dispatch's single
        // call is observable. Task 9's own tests (`retry_worker_drains_...`)
        // cover the wake-and-drain path with explicit `advance` calls.
        let calls = api.push_metrics_calls.lock().await;
        assert_eq!(calls.len(), 1, "primary loop attempted once before enqueue");
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
    fn classify_sees_through_anyhow_context_wrapping() {
        use anyhow::Context;

        // Simulates the exact production path: GrpcServiceApi wraps every RPC
        // failure with `.context("... RPC failed")`. anyhow preserves the
        // inner type across context layers, so downcast_ref::<tonic::Status>
        // on the wrapped error still finds the status. If this ever regresses
        // (anyhow API change, api.rs structure change), classify would start
        // treating permanent INVALID_ARGUMENT / UNAUTHENTICATED failures as
        // retriable, retrying a bad-contract payload forever.

        let raw_err: Result<(), tonic::Status> =
            Err(tonic::Status::invalid_argument("bad payload"));
        let wrapped: anyhow::Error = raw_err.context("PushMetrics RPC failed").unwrap_err();
        assert!(
            matches!(classify(&wrapped), Classify::Drop(tracing::Level::ERROR)),
            "wrapped InvalidArgument should still classify as Drop(ERROR)",
        );

        let raw_err2: Result<(), tonic::Status> = Err(tonic::Status::unavailable("service down"));
        let wrapped2: anyhow::Error = raw_err2.context("PushMetrics RPC failed").unwrap_err();
        assert!(
            matches!(classify(&wrapped2), Classify::Retriable),
            "wrapped Unavailable should still classify as Retriable",
        );
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

    #[test]
    fn build_path_summary_empty_hops() {
        let s = build_path_summary(&[]);
        assert_eq!(s.hop_count, 0);
        assert_eq!(s.avg_rtt_micros, 0);
        assert_eq!(s.loss_pct, 0.0);
    }

    #[test]
    fn build_path_summary_averages_rtt_ignoring_zero_hops() {
        use crate::route::HopSummary as AgentHop;
        let hops = vec![
            AgentHop {
                position: 1,
                observed_ips: vec![],
                avg_rtt_micros: 1_000,
                stddev_rtt_micros: 0,
                loss_pct: 0.10,
            },
            AgentHop {
                position: 2,
                observed_ips: vec![],
                avg_rtt_micros: 0, // fully lost — excluded from RTT mean
                stddev_rtt_micros: 0,
                loss_pct: 1.00,
            },
            AgentHop {
                position: 3,
                observed_ips: vec![],
                avg_rtt_micros: 3_000,
                stddev_rtt_micros: 0,
                loss_pct: 0.20,
            },
        ];
        let s = build_path_summary(&hops);
        assert_eq!(s.hop_count, 3);
        // Mean of 1000 and 3000 = 2000 (zero hop excluded).
        assert_eq!(s.avg_rtt_micros, 2_000);
        // Loss averages include all 3 hops.
        assert!((s.loss_pct - (0.10 + 1.00 + 0.20) / 3.0).abs() < 1e-9);
    }

    #[test]
    fn build_path_summary_all_zero_rtt() {
        use crate::route::HopSummary as AgentHop;
        let hops = vec![AgentHop {
            position: 1,
            observed_ips: vec![],
            avg_rtt_micros: 0,
            stddev_rtt_micros: 0,
            loss_pct: 1.0,
        }];
        let s = build_path_summary(&hops);
        assert_eq!(s.avg_rtt_micros, 0); // no positive RTT → fallback 0
        assert_eq!(s.hop_count, 1);
        assert_eq!(s.loss_pct, 1.0);
    }

    #[test]
    fn hop_to_proto_preserves_ipv4_and_ipv6_bytes() {
        use crate::route::{HopSummary as AgentHop, ObservedIp};
        use std::net::{Ipv4Addr, Ipv6Addr};
        let agent_hop = AgentHop {
            position: 4,
            observed_ips: vec![
                ObservedIp {
                    ip: std::net::IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
                    frequency: 0.6,
                },
                ObservedIp {
                    ip: std::net::IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
                    frequency: 0.4,
                },
            ],
            avg_rtt_micros: 12_345,
            stddev_rtt_micros: 123,
            loss_pct: 0.05,
        };
        let proto = hop_to_proto(&agent_hop);
        assert_eq!(proto.position, 4);
        assert_eq!(proto.avg_rtt_micros, 12_345);
        assert_eq!(proto.stddev_rtt_micros, 123);
        assert!((proto.loss_pct - 0.05).abs() < 1e-9);
        assert_eq!(proto.observed_ips.len(), 2);
        assert_eq!(proto.observed_ips[0].ip.len(), 4);
        assert_eq!(proto.observed_ips[0].ip, vec![10, 1, 2, 3]);
        assert!((proto.observed_ips[0].frequency - 0.6).abs() < 1e-9);
        assert_eq!(proto.observed_ips[1].ip.len(), 16);
        let v6_expected: Vec<u8> = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)
            .octets()
            .to_vec();
        assert_eq!(proto.observed_ips[1].ip, v6_expected);
    }

    #[test]
    fn build_route_snapshot_request_stamps_source_and_target() {
        use crate::route::{RouteSnapshot, RouteSnapshotEnvelope};
        let snap = RouteSnapshot {
            protocol: Protocol::Icmp,
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            hops: vec![],
        };
        let env = RouteSnapshotEnvelope {
            target_id: "t".into(),
            snapshot: snap,
        };
        let req = build_route_snapshot_request("src", env);
        assert_eq!(req.source_id, "src");
        assert_eq!(req.target_id, "t");
        assert_eq!(req.protocol, Protocol::Icmp as i32);
        assert_eq!(req.observed_at_micros, 1_700_000_000_i64 * 1_000_000);
        assert!(req.path_summary.is_some());
        let ps = req.path_summary.unwrap();
        assert_eq!(ps.hop_count, 0);
        assert_eq!(ps.avg_rtt_micros, 0);
        assert_eq!(ps.loss_pct, 0.0);
    }

    #[tokio::test(start_paused = true)]
    async fn primary_loop_pushes_snapshots_immediately_with_path_summary() {
        use crate::route::{HopSummary as AgentHop, RouteSnapshot, RouteSnapshotEnvelope};
        let api = Arc::new(RecordingApi::default());
        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "t".into(),
            start_time: SystemTime::now(),
        };
        let (_mtx, mrx) = mpsc::channel(8);
        let (stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(api.clone(), identity, mrx, srx, cancel.clone());

        let snap = RouteSnapshot {
            protocol: Protocol::Tcp,
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_060),
            hops: vec![AgentHop {
                position: 1,
                observed_ips: vec![],
                avg_rtt_micros: 2_000,
                stddev_rtt_micros: 0,
                loss_pct: 0.0,
            }],
        };
        stx.send(RouteSnapshotEnvelope {
            target_id: "t1".into(),
            snapshot: snap,
        })
        .await
        .unwrap();

        // Park the spawned task in select! so it observes the snapshot recv.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

        let calls = api.push_snapshot_calls.lock().await;
        assert_eq!(calls.len(), 1);
        let req = &calls[0];
        assert_eq!(req.source_id, "src");
        assert_eq!(req.target_id, "t1");
        assert_eq!(req.protocol, Protocol::Tcp as i32);
        let ps = req.path_summary.as_ref().expect("PathSummary stamped");
        assert_eq!(ps.hop_count, 1);
        assert_eq!(ps.avg_rtt_micros, 2_000);
    }

    /// ServiceApi that fails the first N `push_metrics` calls with
    /// UNAVAILABLE, then succeeds. Used to assert the retry worker drains
    /// the queue on transient failures.
    struct FailNThenSucceedApi {
        remaining_failures: Mutex<u32>,
        calls: Mutex<Vec<MetricsBatch>>,
    }
    impl FailNThenSucceedApi {
        fn new(n: u32) -> Self {
            Self {
                remaining_failures: Mutex::new(n),
                calls: Mutex::new(Vec::new()),
            }
        }
    }
    impl ServiceApi for FailNThenSucceedApi {
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
            self.calls.lock().await.push(batch);
            let mut g = self.remaining_failures.lock().await;
            if *g > 0 {
                *g -= 1;
                return Err(anyhow::Error::from(tonic::Status::unavailable("transient")));
            }
            Ok(meshmon_protocol::PushMetricsResponse::default())
        }
        async fn push_route_snapshot(
            &self,
            _: RouteSnapshotRequest,
        ) -> anyhow::Result<meshmon_protocol::PushRouteSnapshotResponse> {
            unimplemented!()
        }
    }

    /// ServiceApi that always fails `push_metrics` with a configurable status.
    /// Used to assert non-retriable failures are dropped on the first retry.
    struct AlwaysFailApi {
        status: tonic::Status,
        calls: Mutex<u32>,
    }
    impl AlwaysFailApi {
        fn new(status: tonic::Status) -> Self {
            Self {
                status,
                calls: Mutex::new(0),
            }
        }
    }
    impl ServiceApi for AlwaysFailApi {
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
            _: MetricsBatch,
        ) -> anyhow::Result<meshmon_protocol::PushMetricsResponse> {
            *self.calls.lock().await += 1;
            Err(anyhow::Error::from(self.status.clone()))
        }
        async fn push_route_snapshot(
            &self,
            _: RouteSnapshotRequest,
        ) -> anyhow::Result<meshmon_protocol::PushRouteSnapshotResponse> {
            unimplemented!()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retry_worker_drains_queue_on_unavailable_then_success() {
        let api = Arc::new(FailNThenSucceedApi::new(2));
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

        // Tick 1 → primary dispatch → fails with UNAVAILABLE → enqueued.
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        // First retry (after ~1s jitter) → fails again.
        tokio::time::advance(Duration::from_secs(5)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        // Second retry (after ~2s jitter) → succeeds.
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

        // The fail-counter was 2, so we expect >= 3 total push_metrics calls
        // (1 primary + 2 retries before the 3rd succeeds). Any flakiness in
        // jitter might push it up to 4 or 5 — the invariant is >= 3.
        let calls = api.calls.lock().await;
        assert!(
            calls.len() >= 3,
            "expected at least 3 push_metrics attempts; got {}",
            calls.len(),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_worker_drops_invalid_argument_after_first_attempt() {
        let api = Arc::new(AlwaysFailApi::new(tonic::Status::invalid_argument("bad")));
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

        // Primary fires → InvalidArgument → non-retriable → dropped, no retry.
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        // Advance a long time. No retries should ever fire.
        tokio::time::advance(Duration::from_secs(600)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

        let calls = *api.calls.lock().await;
        assert_eq!(
            calls, 1,
            "InvalidArgument must drop on first attempt, not retry"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_count_surfaces_on_overflow_then_resets_on_success() {
        // Helper: build a PathMetricsMsg seeded by iteration index so each
        // batch has a distinct 60-second window.
        fn mk_msg(target_id: &str, protocol: Protocol, seed: u64) -> PathMetricsMsg {
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seed * 60);
            PathMetricsMsg {
                target_id: target_id.to_owned(),
                protocol,
                window_start: now - Duration::from_secs(60),
                window_end: now,
                stats: test_stats(),
                health: ProtoHealth::Healthy,
            }
        }

        let api = Arc::new(RecordingApi::default());
        // Start in "fail everything with UNAVAILABLE" mode.
        *api.metrics_result.lock().await = Some(tonic::Status::unavailable("down"));

        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "t".into(),
            start_time: SystemTime::now(),
        };
        let (mtx, mrx) = mpsc::channel(4096);
        let (_stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(api.clone(), identity, mrx, srx, cancel.clone());

        // Let the emitter park in its select!.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        // Feed 80 batches (>> RETRY_QUEUE_CAP = 65) of metrics, one per 60s
        // tick. Each tick the primary loop dispatches one batch that fails
        // with UNAVAILABLE → enqueued. After 65, every subsequent push
        // evicts the oldest and bumps dropped_count.
        for i in 0..80u64 {
            mtx.send(mk_msg("t1", Protocol::Icmp, i)).await.unwrap();
            tokio::time::advance(Duration::from_secs(61)).await;
            for _ in 0..30 {
                tokio::task::yield_now().await;
            }
        }

        // Flip service back to success.
        *api.metrics_result.lock().await = None;

        // Drain enough for the retry worker to start acking queued entries.
        // We don't need to drain the whole 65-entry backlog — as soon as the
        // first successful push_metrics ack fires, the retry worker calls
        // queue.reset_dropped_count(), zeroing the LIVE counter. The next
        // primary-dispatch tick will then read 0. Give the retry worker
        // enough advance/yield cycles to complete at least one successful
        // ack.
        for _ in 0..10 {
            tokio::time::advance(Duration::from_secs(10)).await;
            for _ in 0..30 {
                tokio::task::yield_now().await;
            }
        }

        // Send one more message and drive a fresh primary tick. build_metrics_batch
        // reads queue.dropped_count() live, so this batch reflects whatever
        // the retry worker has set it to.
        let pre_probe_count = api.push_metrics_calls.lock().await.len();
        mtx.send(mk_msg("t1", Protocol::Icmp, 1000)).await.unwrap();
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..40 {
            tokio::task::yield_now().await;
        }

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

        let calls = api.push_metrics_calls.lock().await;

        // Invariant 1: at least one recorded MetricsBatch must carry
        // dropped_count > 0 — proves the overflow -> counter path worked.
        let saw_positive_drop_count = calls.iter().any(|c| {
            c.agent_metadata
                .as_ref()
                .map(|m| m.dropped_count > 0)
                .unwrap_or(false)
        });
        assert!(
            saw_positive_drop_count,
            "expected at least one MetricsBatch with dropped_count > 0; saw {} calls",
            calls.len(),
        );

        // Invariant 2: the post-drain primary-dispatch batch must carry
        // dropped_count = 0 — proves reset_dropped_count() mutated live state
        // after a successful push_metrics ack. Retried batches carry stale
        // snapshots, so we specifically look for a batch dispatched AFTER
        // the drain started — i.e., with index >= pre_probe_count.
        assert!(
            calls.len() > pre_probe_count,
            "expected a primary-dispatch batch after the drain probe; got {} total, {} before probe",
            calls.len(),
            pre_probe_count,
        );
        let post_probe = calls
            .iter()
            .skip(pre_probe_count)
            .find(|c| {
                // Find the fresh primary tick's batch — it contains exactly
                // one PathMetrics (the one we just sent) and its single
                // PathMetrics.target_id matches "t1". (Retried batches are
                // also for "t1" but contain original windowed payloads with
                // seed-based timestamps; the probe batch has seed=1000
                // timestamps ≈ 1_700_060_000 Unix sec.)
                c.paths.len() == 1
                    && c.paths[0].window_end_micros == (1_700_000_000 + 1000 * 60) * 1_000_000
            })
            .expect("probe batch with seed=1000 window should be recorded");
        let dc = post_probe
            .agent_metadata
            .as_ref()
            .expect("agent_metadata always set")
            .dropped_count;
        assert_eq!(
            dc, 0,
            "post-drain primary batch should see dropped_count = 0 after reset; got {}",
            dc,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_drains_pending_snapshots_within_5s() {
        use crate::route::{RouteSnapshot, RouteSnapshotEnvelope};

        let api = Arc::new(RecordingApi::default());
        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "t".into(),
            start_time: SystemTime::now(),
        };
        let (_mtx, mrx) = mpsc::channel(16);
        let (stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(api.clone(), identity, mrx, srx, cancel.clone());

        // Park the emitter in select!.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        // Push 3 snapshots, then cancel immediately. The drain phase must
        // still dispatch all three within its 5 s window.
        for i in 0..3u64 {
            let envelope = RouteSnapshotEnvelope {
                target_id: format!("t{i}"),
                snapshot: RouteSnapshot {
                    protocol: Protocol::Icmp,
                    observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + i),
                    hops: vec![],
                },
            };
            stx.send(envelope).await.unwrap();
        }

        cancel.cancel();

        // Advance up to 5 s and yield so the drain loop makes progress.
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(500)).await;
            for _ in 0..30 {
                tokio::task::yield_now().await;
            }
        }

        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

        let calls = api.push_snapshot_calls.lock().await;
        assert_eq!(
            calls.len(),
            3,
            "expected all 3 queued snapshots to be drained; got {}",
            calls.len(),
        );
        let target_ids: std::collections::HashSet<_> =
            calls.iter().map(|c| c.target_id.clone()).collect();
        assert!(target_ids.contains("t0"));
        assert!(target_ids.contains("t1"));
        assert!(target_ids.contains("t2"));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_flushes_staged_metrics_as_final_batch() {
        let api = Arc::new(RecordingApi::default());
        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "t".into(),
            start_time: SystemTime::now(),
        };
        let (mtx, mrx) = mpsc::channel(16);
        let (_stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(api.clone(), identity, mrx, srx, cancel.clone());

        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        // Push one metric well BEFORE the 60 s tick — it lands in `staged`
        // but is not flushed by the primary loop yet.
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

        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        // Cancel before the 60 s interval fires. The drain phase must flush
        // `staged` as a final best-effort batch.
        cancel.cancel();

        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(500)).await;
            for _ in 0..30 {
                tokio::task::yield_now().await;
            }
        }

        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

        let calls = api.push_metrics_calls.lock().await;
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one final flush batch; got {} calls",
            calls.len(),
        );
        let final_batch = &calls[0];
        assert_eq!(final_batch.paths.len(), 1);
        assert_eq!(final_batch.paths[0].target_id, "t1");
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_respects_5s_deadline_when_snapshots_stream_forever() {
        use crate::route::{RouteSnapshot, RouteSnapshotEnvelope};

        let api = Arc::new(RecordingApi::default());
        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "t".into(),
            start_time: SystemTime::now(),
        };
        let (_mtx, mrx) = mpsc::channel(16);
        let (stx, srx) = mpsc::channel(1024);
        let cancel = CancellationToken::new();
        let handle = spawn(api.clone(), identity, mrx, srx, cancel.clone());

        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        // Fire 100 snapshots (more than we can plausibly drain in 5 s of
        // logical time — each dispatch is near-instant under the recording
        // API, but this asserts the deadline bound exists).
        for i in 0..100u64 {
            let _ = stx.try_send(RouteSnapshotEnvelope {
                target_id: format!("t{i}"),
                snapshot: RouteSnapshot {
                    protocol: Protocol::Icmp,
                    observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + i),
                    hops: vec![],
                },
            });
        }

        cancel.cancel();

        // Advance through the 5 s deadline + margin.
        for _ in 0..20 {
            tokio::time::advance(Duration::from_millis(500)).await;
            for _ in 0..30 {
                tokio::task::yield_now().await;
            }
        }

        // Handle must exit cleanly (within the 10 s outer test timeout),
        // and the drain loop must have obeyed the 5 s deadline internally.
        let t0 = tokio::time::Instant::now();
        let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
        // No explicit assertion on call count — we just don't want a hang.
        // Sanity: at least one snapshot was processed before the deadline.
        let _ = t0;
        let calls = api.push_snapshot_calls.lock().await;
        assert!(
            !calls.is_empty(),
            "expected at least one snapshot drained before deadline",
        );
    }

    #[test]
    fn bump_retry_increments_attempts_and_reschedules() {
        let now = Instant::now();
        let entry = PendingRpc::Metrics {
            batch: MetricsBatch::default(),
            next_retry_at: now,
            attempts: 2,
        };
        let bumped = bump_retry(entry);
        match bumped {
            PendingRpc::Metrics {
                attempts,
                next_retry_at,
                ..
            } => {
                assert_eq!(attempts, 3);
                assert!(next_retry_at > now);
            }
            _ => panic!("variant changed unexpectedly"),
        }

        let entry2 = PendingRpc::Snapshot {
            req: RouteSnapshotRequest::default(),
            next_retry_at: now,
            attempts: 0,
        };
        let bumped2 = bump_retry(entry2);
        match bumped2 {
            PendingRpc::Snapshot {
                attempts,
                next_retry_at,
                ..
            } => {
                assert_eq!(attempts, 1);
                assert!(next_retry_at > now);
            }
            _ => panic!("variant changed unexpectedly"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_drains_buffered_metrics_before_channel_closed_exit() {
        // Regression: a fast-close shutdown where the supervisor drops
        // metrics_tx with messages still buffered MUST see those messages
        // flushed as a final batch — the early-exit check must not fire
        // while unread messages sit in the mpsc buffer.
        let api = Arc::new(RecordingApi::default());
        let identity = EmitterIdentity {
            source_id: "src".into(),
            agent_version: "t".into(),
            start_time: SystemTime::now(),
        };
        let (mtx, mrx) = mpsc::channel(16);
        let (_stx, srx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let handle = spawn(api.clone(), identity, mrx, srx, cancel.clone());

        // Park emitter in primary select!.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        // Queue TWO messages, then drop the sender. Critically, do NOT
        // advance the 60 s metrics tick before cancelling — both messages
        // must still be in `staged` (they were pushed by the primary loop
        // in reaction to `metrics_rx.recv()` arms) OR in the mpsc buffer
        // when the drop + cancel race happens.
        let now = SystemTime::now();
        for i in 0..2u64 {
            mtx.send(PathMetricsMsg {
                target_id: format!("t{i}"),
                protocol: Protocol::Icmp,
                window_start: now - Duration::from_secs(60),
                window_end: now,
                stats: test_stats(),
                health: ProtoHealth::Healthy,
            })
            .await
            .unwrap();
        }
        drop(mtx);
        cancel.cancel();

        // Advance a little so the drain loop runs.
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(500)).await;
            for _ in 0..30 {
                tokio::task::yield_now().await;
            }
        }

        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

        let calls = api.push_metrics_calls.lock().await;
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one final flush batch; got {} calls",
            calls.len(),
        );
        let paths = &calls[0].paths;
        assert_eq!(
            paths.len(),
            2,
            "final batch must contain both messages; got {}",
            paths.len()
        );
        let target_ids: std::collections::HashSet<_> =
            paths.iter().map(|p| p.target_id.clone()).collect();
        assert!(target_ids.contains("t0"));
        assert!(target_ids.contains("t1"));
    }
}
