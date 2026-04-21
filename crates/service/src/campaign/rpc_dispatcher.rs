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
use super::model::{MeasurementKind, ProbeProtocol};
use super::writer::{SettleOutcome, SettleWriter};
use crate::metrics;
use crate::registry::AgentRegistry;
use async_trait::async_trait;
use dashmap::DashMap;
use futures_util::StreamExt;
use meshmon_protocol::{
    AgentCommandClient, MeasurementKind as WireMeasurementKind, MeasurementTarget, Protocol,
    RunMeasurementBatchRequest,
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

    /// Return an `Arc<Semaphore>` sized for `effective`. See
    /// [`resize_or_init_agent_semaphore`] for the grow-but-don't-shrink
    /// rationale.
    fn semaphore_for(&self, agent_id: &str, effective: u32) -> Arc<Semaphore> {
        resize_or_init_agent_semaphore(&self.agent_semaphores, agent_id, effective)
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
    /// batch shares the same campaign AND the same [`MeasurementKind`]
    /// (scheduler invariant: `take_pending_batch` is per-`(campaign,
    /// agent)` and `Scheduler::dispatch_for_campaign` splits the claim
    /// by `kind` before calling the dispatcher), so per-campaign knobs
    /// and the wire `MeasurementKind` come from the first pair.
    fn build_request(&self, allowed: &[PendingPair]) -> RunMeasurementBatchRequest {
        let head = &allowed[0];
        // Detail MTR rows (`kind = DetailMtr`) map to the wire MTR
        // measurement. Campaign and detail-ping rows both run the
        // latency path with their respective `probe_count`.
        let kind = match head.kind {
            MeasurementKind::DetailMtr => WireMeasurementKind::Mtr,
            MeasurementKind::Campaign | MeasurementKind::DetailPing => WireMeasurementKind::Latency,
        };
        let protocol = match head.protocol {
            ProbeProtocol::Icmp => Protocol::Icmp,
            ProbeProtocol::Tcp => Protocol::Tcp,
            ProbeProtocol::Udp => Protocol::Udp,
        };
        // `destination_port = 0` is correct for the ICMP path shipped
        // in T45 (ICMP has no port). TCP/UDP campaigns will need the
        // per-target destination port populated here — that's T46
        // scope (it lands together with the real trippy-backed prober
        // and the `PendingPair::destination_port` field). Until then,
        // the `[campaigns] enabled = false` default plus the agent's
        // port-ignoring `StubProber` keep the hardcoded zero from
        // causing production harm. A TCP/UDP campaign created today
        // would only probe port 0 once T46 flips both gates AND fails
        // to populate the port — which the T46 plan explicitly covers.
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

/// Return the per-agent semaphore sized for `effective`, growing the
/// existing entry via `Semaphore::add_permits` when the cap widens but
/// never shrinking it. Replacing the semaphore on shrink would leak
/// accounting for permits held by in-flight dispatches and briefly let
/// total concurrency exceed the new cap — `tokio::sync::Semaphore`
/// offers no safe way to revoke already-issued permits. Shrinks
/// therefore take effect only on service restart; that tradeoff is
/// documented in the agent concurrency section of the campaigns docs.
fn resize_or_init_agent_semaphore(
    semaphores: &DashMap<String, AgentSemaphore>,
    agent_id: &str,
    effective: u32,
) -> Arc<Semaphore> {
    use dashmap::mapref::entry::Entry;
    match semaphores.entry(agent_id.to_string()) {
        Entry::Occupied(mut e) => {
            let slot = e.get_mut();
            if effective > slot.permits {
                slot.semaphore
                    .add_permits((effective - slot.permits) as usize);
                slot.permits = effective;
            }
            slot.semaphore.clone()
        }
        Entry::Vacant(e) => {
            let fresh = AgentSemaphore {
                semaphore: Arc::new(Semaphore::new(effective as usize)),
                permits: effective,
            };
            let semaphore = fresh.semaphore.clone();
            e.insert(fresh);
            semaphore
        }
    }
}

/// Short stable string used as the `kind` label on the dispatch metric.
fn kind_label(req: &RunMeasurementBatchRequest) -> &'static str {
    match WireMeasurementKind::try_from(req.kind).unwrap_or(WireMeasurementKind::Latency) {
        WireMeasurementKind::Mtr => "mtr",
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
                rate_limited_ids: Vec::new(),
                skipped_reason: Some("agent_unreachable".into()),
            };
        };

        // 2. Per-agent semaphore. Use `try_acquire_owned`, not the
        // awaiting variant: the scheduler dispatches agents serially
        // per tick, so blocking here on a saturated agent would stall
        // every other agent's dispatch behind it. Saturation lands
        // in `rate_limited_ids` (same "pre-RPC throttling" category
        // as the per-destination bucket) so the scheduler reverts to
        // `pending` AND decrements `attempt_count` — declining to
        // dispatch because the cap is saturated must not burn retry
        // budget.
        let effective = self.effective_concurrency(agent_id);
        let semaphore = self.semaphore_for(agent_id, effective);
        let _permit = match semaphore.try_acquire_owned() {
            Ok(p) => p,
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                return DispatchOutcome {
                    dispatched: 0,
                    rejected_ids: Vec::new(),
                    rate_limited_ids: batch.iter().map(|p| p.pair_id).collect(),
                    skipped_reason: Some("agent_busy".into()),
                };
            }
            Err(tokio::sync::TryAcquireError::Closed) => {
                return DispatchOutcome {
                    dispatched: 0,
                    rejected_ids: batch.iter().map(|p| p.pair_id).collect(),
                    rate_limited_ids: Vec::new(),
                    skipped_reason: Some("semaphore_closed".into()),
                };
            }
        };

        // 3. Per-destination rate limit. Bucket-rejected pairs go into
        // `rate_limited_ids` (not `rejected_ids`) so the scheduler
        // reverts them WITH `attempt_count--` — a throttling decision
        // before the RPC should not consume retry budget.
        let (allowed, rate_limited) = self.reserve_tokens(batch).await;
        if allowed.is_empty() {
            return DispatchOutcome {
                dispatched: 0,
                rejected_ids: Vec::new(),
                rate_limited_ids: rate_limited,
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
                let rejected: Vec<i64> = allowed.iter().map(|p| p.pair_id).collect();
                metrics::campaign_batches_total(agent_id, kind, "rpc_error").increment(1);
                metrics::campaign_batch_duration_seconds(agent_id, kind)
                    .record(start.elapsed().as_secs_f64());
                return DispatchOutcome {
                    dispatched: 0,
                    rejected_ids: rejected,
                    rate_limited_ids: rate_limited,
                    skipped_reason: Some(format!("rpc_error:{}", status.code())),
                };
            }
        };

        // 6. Drain stream into writer.
        let mut expected: HashMap<i64, PendingPair> =
            allowed.into_iter().map(|p| (p.pair_id, p)).collect();
        let mut rejected: Vec<i64> = Vec::new();
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
        let has_revert = !rejected.is_empty() || !rate_limited.is_empty();
        let outcome_label = if has_revert { "partial" } else { "ok" };
        metrics::campaign_batches_total(agent_id, kind, outcome_label).increment(1);
        metrics::campaign_batch_duration_seconds(agent_id, kind)
            .record(start.elapsed().as_secs_f64());

        DispatchOutcome {
            dispatched: dispatched_ok,
            rejected_ids: rejected,
            rate_limited_ids: rate_limited,
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
            kind: WireMeasurementKind::Mtr as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 1,
            timeout_ms: 0,
            probe_stagger_ms: 0,
            targets: vec![],
        };
        assert_eq!(kind_label(&req), "mtr");
        let req = RunMeasurementBatchRequest {
            kind: WireMeasurementKind::Latency as i32,
            ..req
        };
        assert_eq!(kind_label(&req), "latency");
    }

    fn sample_pending(pair_id: i64, kind: MeasurementKind, probe_count: i16) -> PendingPair {
        PendingPair {
            pair_id,
            campaign_id: uuid::Uuid::nil(),
            source_agent_id: "agent-x".into(),
            destination_ip: "198.51.100.7".parse().unwrap(),
            probe_count,
            timeout_ms: 2_000,
            probe_stagger_ms: 100,
            force_measurement: false,
            protocol: ProbeProtocol::Icmp,
            kind,
        }
    }

    fn fresh_dispatcher() -> RpcDispatcher {
        let tunnels = Arc::new(meshmon_revtunnel::TunnelManager::new());
        let registry = Arc::new(AgentRegistry::new(
            sqlx::PgPool::connect_lazy("postgres://unused").expect("lazy pool"),
            Duration::from_secs(60),
            Duration::from_secs(300),
        ));
        let writer =
            SettleWriter::new(sqlx::PgPool::connect_lazy("postgres://unused").expect("lazy pool"));
        RpcDispatcher::new(
            tunnels, registry, writer, /*default*/ 4, /*rps*/ 100, 1024,
        )
    }

    #[tokio::test]
    async fn build_request_kind_follows_pair_kind() {
        let disp = fresh_dispatcher();
        // DetailMtr → wire MTR.
        let req = disp.build_request(&[sample_pending(1, MeasurementKind::DetailMtr, 1)]);
        assert_eq!(req.kind, WireMeasurementKind::Mtr as i32);
        // Campaign → wire Latency even when probe_count happens to be 1.
        let req = disp.build_request(&[sample_pending(2, MeasurementKind::Campaign, 1)]);
        assert_eq!(req.kind, WireMeasurementKind::Latency as i32);
        // DetailPing → wire Latency.
        let req = disp.build_request(&[sample_pending(3, MeasurementKind::DetailPing, 250)]);
        assert_eq!(req.kind, WireMeasurementKind::Latency as i32);
    }

    #[test]
    fn ip_to_bytes_produces_canonical_width() {
        assert_eq!(ip_to_bytes("10.0.0.1".parse().unwrap()).len(), 4);
        assert_eq!(ip_to_bytes("::1".parse().unwrap()).len(), 16);
    }

    #[test]
    fn semaphore_cache_initializes_at_requested_cap() {
        let cache = DashMap::new();
        let sem = resize_or_init_agent_semaphore(&cache, "a", 3);
        assert_eq!(sem.available_permits(), 3);
        assert_eq!(cache.get("a").unwrap().permits, 3);
    }

    #[test]
    fn semaphore_cache_grows_on_widened_cap() {
        let cache = DashMap::new();
        let first = resize_or_init_agent_semaphore(&cache, "a", 2);
        let second = resize_or_init_agent_semaphore(&cache, "a", 5);
        // Same Arc — the semaphore was grown in place, not replaced.
        // Crucial: held permits on `first` remain valid.
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.available_permits(), 5);
        assert_eq!(cache.get("a").unwrap().permits, 5);
    }

    #[test]
    fn semaphore_cache_ignores_shrink_requests() {
        // A shrink would leak accounting for any in-flight batch still
        // holding permits on the old semaphore, so we keep the old
        // instance. Operators wanting to shrink the cap restart.
        let cache = DashMap::new();
        let first = resize_or_init_agent_semaphore(&cache, "a", 4);
        let second = resize_or_init_agent_semaphore(&cache, "a", 1);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.available_permits(), 4, "shrink must be ignored");
        assert_eq!(cache.get("a").unwrap().permits, 4);
    }

    #[tokio::test]
    async fn semaphore_cache_grow_preserves_held_permits() {
        // Simulate the real race: acquire permits on the small
        // semaphore, then the operator raises the cap. Outstanding
        // permits must still count — total in-flight cannot exceed
        // the new cap.
        let cache = DashMap::new();
        let sem = resize_or_init_agent_semaphore(&cache, "a", 2);
        let held1 = sem.clone().acquire_owned().await.unwrap();
        let held2 = sem.clone().acquire_owned().await.unwrap();
        assert_eq!(sem.available_permits(), 0);

        let _grown = resize_or_init_agent_semaphore(&cache, "a", 4);
        // The old two permits are still outstanding; only two fresh
        // permits are issuable. Taking three would block the third.
        assert_eq!(sem.available_permits(), 2);

        drop(held1);
        drop(held2);
        assert_eq!(sem.available_permits(), 4);
    }
}
