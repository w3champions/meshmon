//! Real RPC-backed [`PairDispatcher`] implementation.
//!
//! Replaces the `NoopDispatcher` in production (see Task 13 for the
//! `main.rs` wiring). Test-only stubs (`NoopDispatcher`,
//! `DirectSettleDispatcher`) stay in `dispatch.rs`.
//!
//! Per-agent flow:
//!   1. Borrow the per-agent `tonic::Channel` from
//!      [`meshmon_revtunnel::TunnelManager`]. Missing tunnel → whole
//!      batch is rejected with `skipped_reason = "agent_unreachable"`.
//!   2. Acquire a permit on a **per-agent** [`tokio::sync::Semaphore`]
//!      sized from `AgentInfo.campaign_max_concurrency` (cluster default
//!      otherwise). The semaphore is cached and rebuilt only when the
//!      effective concurrency changes.
//!   3. Reserve per-destination tokens from a **process-wide**
//!      leaky-bucket cache. Pairs that lose the draw never reach the
//!      agent and join `rejected_ids`.
//!   4. Open the server-streaming `RunMeasurementBatch` RPC.
//!   5. Drain each [`MeasurementResult`] into
//!      [`super::writer::SettleWriter::settle`]. The writer owns the
//!      `campaign_pairs` terminal-state update and the
//!      `campaign_pair_settled` NOTIFY.
//!   6. Populate [`DispatchOutcome::rejected_ids`] with:
//!        - every pair the agent never produced a result for (stream
//!          drop, mid-stream RPC error),
//!        - every pair blocked by the per-destination bucket.
//!
//!      Terminal agent-reported failures (`MeasurementFailure`) are
//!      **settled** by the writer, not rejected — the writer maps the
//!      failure code to `skipped`/`unreachable` with a `last_error` tag.
//!
//! Cancellation: the dispatcher is invoked by the scheduler. If the
//! scheduler's cancellation token fires, its task drops this future,
//! which drops the tonic response stream, which in turn closes the
//! HTTP/2 stream with `CANCEL`. The agent's handler observes the cancel
//! and winds down its in-flight probes.

use super::dispatch::{DispatchOutcome, PairDispatcher, PendingPair};
use super::model::ProbeProtocol;
use super::writer::{SettleOutcome, SettleWriter};
use crate::metrics;
use crate::registry::AgentRegistry;
use async_trait::async_trait;
use dashmap::DashMap;
use futures_util::StreamExt;
use meshmon_protocol::{
    AgentCommandClient, MeasurementKind, MeasurementTarget, Protocol, RunMeasurementBatchRequest,
};
use meshmon_revtunnel::TunnelManager;
use moka::future::Cache;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, warn};

/// Per-destination leaky bucket used by the process-wide rate limiter.
///
/// Same shape as the T44 `scheduler::Bucket`; kept module-private so
/// the dispatcher owns an independent cache without a cross-module
/// dependency.
#[derive(Debug)]
struct Bucket {
    /// Tokens remaining in the current second.
    remaining: u32,
    /// When the bucket last refilled.
    refilled_at: Instant,
    /// Capacity (tokens per second).
    capacity: u32,
}

impl Bucket {
    fn new(capacity: u32) -> Self {
        Self {
            remaining: capacity,
            refilled_at: Instant::now(),
            capacity,
        }
    }

    /// Try to draw `n` tokens. Returns the number actually drawn (0..=n).
    fn try_take(&mut self, n: u32) -> u32 {
        let now = Instant::now();
        if now.duration_since(self.refilled_at) >= Duration::from_secs(1) {
            self.remaining = self.capacity;
            self.refilled_at = now;
        }
        let drawn = n.min(self.remaining);
        self.remaining -= drawn;
        drawn
    }
}

/// Cached per-agent semaphore + the concurrency it was sized for. We
/// rebuild the semaphore whenever the agent's effective concurrency
/// changes (config hot-reload via Register replay) so an operator
/// tightening the cap actually takes effect on the next dispatch.
#[derive(Clone)]
struct AgentSemaphore {
    semaphore: Arc<Semaphore>,
    /// The value of `effective_concurrency` this semaphore was built
    /// with. A mismatch triggers a rebuild.
    permits: u32,
}

/// Real dispatcher. Cheap to clone (all interior state is `Arc`-owned).
pub struct RpcDispatcher {
    /// Service-side tunnel registry. `channel_for(agent_id)` returns a
    /// cloned `tonic::Channel` routed via the agent's yamux tunnel.
    tunnels: Arc<TunnelManager>,
    /// Agent registry — consulted per-dispatch for per-agent
    /// concurrency overrides.
    registry: Arc<AgentRegistry>,
    /// Owned settle writer. `SettleWriter` is `Clone` via an `Arc` in
    /// its internals, so the dispatcher keeps a private copy rather
    /// than threading a pool through the call site.
    writer: SettleWriter,
    /// Cluster-wide fallback concurrency when an agent has no override.
    default_agent_concurrency: u32,
    /// Per-destination refill rate (tokens per second per IP).
    per_destination_rps: u32,
    /// Hard cap on `MeasurementTarget`s shipped in one request. Extra
    /// pairs are dropped at the `build_request` boundary; the scheduler
    /// uses a smaller `chunk_size` so this is usually a no-op.
    max_batch_size: u32,
    /// Per-agent semaphores. Built lazily on first dispatch per agent
    /// and rebuilt on concurrency changes. `DashMap` so lookup stays
    /// lock-free on the hot path.
    agent_semaphores: Arc<DashMap<String, AgentSemaphore>>,
    /// Process-wide destination-IP token buckets. TTL of 60 s so
    /// unused entries drain — no leak as the destination churn stays
    /// bounded by live campaign target sets.
    buckets: Cache<IpAddr, Arc<Mutex<Bucket>>>,
}

impl RpcDispatcher {
    /// Construct a dispatcher. All arguments are cheap to clone; no
    /// DB or RPC contact happens here.
    pub fn new(
        tunnels: Arc<TunnelManager>,
        registry: Arc<AgentRegistry>,
        writer: SettleWriter,
        default_agent_concurrency: u32,
        per_destination_rps: u32,
        max_batch_size: u32,
    ) -> Self {
        Self {
            tunnels,
            registry,
            writer,
            default_agent_concurrency,
            per_destination_rps,
            max_batch_size,
            agent_semaphores: Arc::new(DashMap::new()),
            buckets: Cache::builder()
                .time_to_idle(Duration::from_secs(60))
                .build(),
        }
    }

    /// Per-agent in-flight concurrency, honouring the override if set.
    fn effective_concurrency(&self, agent_id: &str) -> u32 {
        self.registry
            .snapshot()
            .get(agent_id)
            .and_then(|a| a.campaign_max_concurrency)
            .unwrap_or(self.default_agent_concurrency)
            .max(1)
    }

    /// Return an `Arc<Semaphore>` sized for `effective`. Rebuilds the
    /// cached entry when the requested size changes so a runtime cap
    /// tweak takes effect on the next dispatch.
    fn semaphore_for(&self, agent_id: &str, effective: u32) -> Arc<Semaphore> {
        if let Some(existing) = self.agent_semaphores.get(agent_id) {
            if existing.permits == effective {
                return existing.semaphore.clone();
            }
        }
        let fresh = AgentSemaphore {
            semaphore: Arc::new(Semaphore::new(effective as usize)),
            permits: effective,
        };
        let semaphore = fresh.semaphore.clone();
        self.agent_semaphores.insert(agent_id.to_string(), fresh);
        semaphore
    }

    /// Apply the per-destination rate limit.
    ///
    /// Returns `(allowed, rate_limited_pair_ids)`. Rate-limited pairs
    /// are reverted to `pending` by the scheduler via the normal
    /// `rejected_ids` path.
    ///
    /// Records [`metrics::campaign_dest_bucket_wait_seconds`] with the
    /// wall time each pair spent acquiring (or failing to acquire) a
    /// token — granular enough to expose bucket contention without
    /// flooding the exporter, since each histogram observation covers
    /// one pair rather than a whole batch.
    async fn reserve_tokens(&self, batch: Vec<PendingPair>) -> (Vec<PendingPair>, Vec<i64>) {
        let mut allowed = Vec::with_capacity(batch.len());
        let mut rate_limited = Vec::new();
        for p in batch {
            let dest = p.destination_ip;
            let wait_start = Instant::now();
            let bucket = self
                .buckets
                .get_with(dest, async {
                    Arc::new(Mutex::new(Bucket::new(self.per_destination_rps)))
                })
                .await;
            let drawn = {
                let mut guard = bucket.lock().await;
                guard.try_take(1)
            };
            metrics::campaign_dest_bucket_wait_seconds().record(wait_start.elapsed().as_secs_f64());
            if drawn == 1 {
                allowed.push(p);
            } else {
                rate_limited.push(p.pair_id);
            }
        }
        (allowed, rate_limited)
    }

    /// Build the wire request from the allowed subset. Every pair in a
    /// batch shares the same campaign (scheduler invariant:
    /// `take_pending_batch` is per-`(campaign, agent)`), so per-campaign
    /// knobs come from the first pair.
    fn build_request(&self, allowed: &[PendingPair]) -> RunMeasurementBatchRequest {
        let head = &allowed[0];
        // T45 heuristic: `probe_count == 1` comes from detail MTR
        // re-runs (spec 03 §4.5 forces `probe_count=1` for MTR;
        // campaign default is 10). T48 will introduce an explicit
        // `measurement_kind` on `PendingPair` when detail_mtr campaigns
        // land — until then, `probe_count == 1` is the stable signal.
        let kind = if head.probe_count == 1 {
            MeasurementKind::Mtr
        } else {
            MeasurementKind::Latency
        };
        let protocol = match head.protocol {
            ProbeProtocol::Icmp => Protocol::Icmp,
            ProbeProtocol::Tcp => Protocol::Tcp,
            ProbeProtocol::Udp => Protocol::Udp,
        };
        let targets = allowed
            .iter()
            .take(self.max_batch_size as usize)
            .map(|p| MeasurementTarget {
                pair_id: p.pair_id as u64,
                destination_ip: ip_to_bytes(p.destination_ip).into(),
                destination_port: 0,
            })
            .collect();
        RunMeasurementBatchRequest {
            batch_id: rand::random(),
            kind: kind as i32,
            protocol: protocol as i32,
            probe_count: head.probe_count as u32,
            timeout_ms: head.timeout_ms as u32,
            probe_stagger_ms: head.probe_stagger_ms as u32,
            targets,
        }
    }
}

/// Serialize an `IpAddr` to its canonical 4- or 16-byte payload.
fn ip_to_bytes(ip: IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(v) => v.octets().to_vec(),
        IpAddr::V6(v) => v.octets().to_vec(),
    }
}

/// Short stable string used as the `kind` label on the dispatch metric.
fn kind_label(req: &RunMeasurementBatchRequest) -> &'static str {
    match MeasurementKind::try_from(req.kind).unwrap_or(MeasurementKind::Latency) {
        MeasurementKind::Mtr => "mtr",
        _ => "latency",
    }
}

/// RAII guard that bumps `meshmon_campaign_pairs_inflight` on
/// construction and decrements on drop. Every early-return path out of
/// [`RpcDispatcher::dispatch`] therefore restores the gauge without any
/// explicit bookkeeping — the gauge naturally returns to zero once
/// every live batch for an agent has completed.
struct InflightGuard {
    agent_id: String,
    count: u32,
}

impl InflightGuard {
    fn new(agent_id: &str, count: usize) -> Self {
        let count = count.min(u32::MAX as usize) as u32;
        metrics::campaign_pairs_inflight(agent_id).increment(count as f64);
        Self {
            agent_id: agent_id.to_string(),
            count,
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        metrics::campaign_pairs_inflight(&self.agent_id).decrement(self.count as f64);
    }
}

#[async_trait]
impl PairDispatcher for RpcDispatcher {
    async fn dispatch(&self, agent_id: &str, batch: Vec<PendingPair>) -> DispatchOutcome {
        if batch.is_empty() {
            return DispatchOutcome::default();
        }

        // 1. Resolve tunnel channel.
        let Some(channel) = self.tunnels.channel_for(agent_id) else {
            return DispatchOutcome {
                dispatched: 0,
                rejected_ids: batch.iter().map(|p| p.pair_id).collect(),
                skipped_reason: Some("agent_unreachable".into()),
            };
        };

        // 2. Per-agent semaphore (rebuild on concurrency change).
        let effective = self.effective_concurrency(agent_id);
        let semaphore = self.semaphore_for(agent_id, effective);
        let _permit = match semaphore.acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                return DispatchOutcome {
                    dispatched: 0,
                    rejected_ids: batch.iter().map(|p| p.pair_id).collect(),
                    skipped_reason: Some("semaphore_closed".into()),
                };
            }
        };

        // 3. Per-destination rate limit.
        let (allowed, rate_limited) = self.reserve_tokens(batch).await;
        if allowed.is_empty() {
            return DispatchOutcome {
                dispatched: 0,
                rejected_ids: rate_limited,
                skipped_reason: Some("rate_limited".into()),
            };
        }

        // Track the allowed subset as in-flight until the dispatch
        // function returns; `_inflight` is dropped on every exit path
        // (success, rpc error, mid-stream drop) so the gauge returns to
        // zero naturally.
        let _inflight = InflightGuard::new(agent_id, allowed.len());

        // 4. Build request.
        let req = self.build_request(&allowed);
        let kind = kind_label(&req);

        // 5. Open server-streaming RPC.
        let mut client = AgentCommandClient::new(channel);
        let start = Instant::now();
        let mut stream = match client.run_measurement_batch(req).await {
            Ok(resp) => resp.into_inner(),
            Err(status) => {
                warn!(
                    agent_id,
                    error = %status,
                    "run_measurement_batch RPC failed"
                );
                let mut rejected: Vec<i64> = allowed.iter().map(|p| p.pair_id).collect();
                rejected.extend(rate_limited);
                metrics::campaign_batches_total(agent_id, kind, "rpc_error").increment(1);
                metrics::campaign_batch_duration_seconds(agent_id, kind)
                    .record(start.elapsed().as_secs_f64());
                return DispatchOutcome {
                    dispatched: 0,
                    rejected_ids: rejected,
                    skipped_reason: Some(format!("rpc_error:{}", status.code())),
                };
            }
        };

        // 6. Drain stream into writer.
        let mut expected: HashMap<i64, PendingPair> =
            allowed.into_iter().map(|p| (p.pair_id, p)).collect();
        let mut rejected = rate_limited;
        let mut dispatched_ok = 0usize;

        while let Some(item) = stream.next().await {
            let result = match item {
                Ok(r) => r,
                Err(status) => {
                    warn!(agent_id, error = %status, "mid-stream RPC error");
                    // Every remaining expected pair joins rejected;
                    // already-settled pairs stay settled.
                    rejected.extend(expected.keys().copied());
                    expected.clear();
                    break;
                }
            };
            let pair_id = result.pair_id as i64;
            let Some(pair) = expected.remove(&pair_id) else {
                warn!(
                    agent_id,
                    pair_id, "unexpected pair_id in response; ignoring"
                );
                continue;
            };
            match self.writer.settle(&pair, &result).await {
                Ok(SettleOutcome::Settled) => dispatched_ok += 1,
                Ok(SettleOutcome::RaceLost) => {
                    // Concurrent operator reset landed first; the writer
                    // rolled back. Drop silently — the scheduler owns
                    // the next step for that row.
                    debug!(agent_id, pair_id, "late settle dropped by state gate");
                }
                Ok(SettleOutcome::MalformedNoOutcome) => {
                    // Agent sent a result with no `outcome` field —
                    // protocol violation. Reject so the scheduler
                    // reverts the pair; otherwise it would sit in
                    // `dispatched` forever and block campaign completion.
                    warn!(
                        agent_id,
                        pair_id, "result carried no outcome; reverting pair"
                    );
                    rejected.push(pair_id);
                }
                Err(e) => {
                    warn!(
                        agent_id,
                        pair_id,
                        error = %e,
                        "writer settle failed"
                    );
                    rejected.push(pair_id);
                }
            }
        }
        // Any pair whose result never arrived joins rejected for a retry.
        rejected.extend(expected.keys().copied());

        // `skipped_reason` stays `None` when at least one result landed
        // on the stream (even if the writer rolled it back) — the batch
        // reached the agent and the scheduler should revert only the
        // rejected subset, not the whole thing.
        let outcome_label = if rejected.is_empty() { "ok" } else { "partial" };
        metrics::campaign_batches_total(agent_id, kind, outcome_label).increment(1);
        metrics::campaign_batch_duration_seconds(agent_id, kind)
            .record(start.elapsed().as_secs_f64());

        DispatchOutcome {
            dispatched: dispatched_ok,
            rejected_ids: rejected,
            skipped_reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_refills_on_each_whole_second() {
        let mut b = Bucket::new(2);
        assert_eq!(b.try_take(1), 1);
        assert_eq!(b.try_take(1), 1);
        // Bucket empty; draws return 0 until refill.
        assert_eq!(b.try_take(1), 0);
        // Force a refill by rewinding `refilled_at` past one second.
        b.refilled_at = Instant::now() - Duration::from_secs(2);
        assert_eq!(b.try_take(2), 2);
    }

    #[test]
    fn kind_label_maps_mtr_and_latency() {
        let req = RunMeasurementBatchRequest {
            batch_id: 0,
            kind: MeasurementKind::Mtr as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 1,
            timeout_ms: 0,
            probe_stagger_ms: 0,
            targets: vec![],
        };
        assert_eq!(kind_label(&req), "mtr");
        let req = RunMeasurementBatchRequest {
            kind: MeasurementKind::Latency as i32,
            ..req
        };
        assert_eq!(kind_label(&req), "latency");
    }

    #[test]
    fn ip_to_bytes_produces_canonical_width() {
        assert_eq!(ip_to_bytes("10.0.0.1".parse().unwrap()).len(), 4);
        assert_eq!(ip_to_bytes("::1".parse().unwrap()).len(), 16);
    }
}
