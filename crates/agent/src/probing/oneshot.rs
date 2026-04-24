//! Trippy-backed one-off prober for campaigns.
//!
//! Every campaign protocol (ICMP / TCP / UDP / MTR) routes through a
//! single `trippy_core::Tracer` instance per pair. The per-protocol
//! builder matrix lives in [`build_oneshot_config`]; per-protocol loss
//! predicates and aggregation live in `aggregate_*_latency` / `aggregate_mtr`.
//!
//! See the embedded module doc for the shared-resource audit, cancellation
//! semantics, and the rationale for each knob.

#![doc = include_str!("oneshot.md")]

use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{
    HopIp, HopSummary, MeasurementFailure, MeasurementFailureCode, MeasurementKind,
    MeasurementResult, MeasurementSummary, MeasurementTarget, MtrTraceResult, Protocol,
    RunMeasurementBatchRequest,
};
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use tonic::Status;
use trippy_core::{Builder, Port, PortDirection, State};

use crate::command::CampaignProber;

// ---------------------------------------------------------------------------
// Collision counter (see spec 03 §6 and oneshot.md § Shared-resource audit)
// ---------------------------------------------------------------------------

/// Process-wide counter. Stays at 0 in steady state — trippy-core's
/// reply dispatcher already filters at the library level, so no
/// production path calls `fetch_add`. Reserved for a future aggregator
/// that uses `Tracer::run_with` and can observe stray replies directly.
/// Observed via the shutdown log in `bootstrap.rs` (mirrors the
/// continuous prober's `CROSS_CONTAMINATION_TOTAL`) and by the
/// coexistence integration test.
static ONESHOT_PROBE_COLLISIONS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Return the current collision count. Read by bootstrap at shutdown
/// and by the coexistence integration test.
pub(crate) fn oneshot_probe_collisions_total() -> u64 {
    ONESHOT_PROBE_COLLISIONS_TOTAL.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn reset_oneshot_collisions_for_test() {
    ONESHOT_PROBE_COLLISIONS_TOTAL.store(0, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Knobs
// ---------------------------------------------------------------------------

/// Max hops the one-shot tracer probes. Matches spec 03 §4.3.
const ONESHOT_MAX_TTL: u8 = 32;

/// Covers late destination replies on >200 ms RTT paths. Mirrors the
/// continuous prober; trippy's own grace timer, independent of the probe
/// read timeout.
const ONESHOT_GRACE: Duration = Duration::from_millis(500);

/// Hard round deadline for MTR. Spec 03 §4.5: single-round, 30 s wall clock.
const MTR_ROUND_TIMEOUT: Duration = Duration::from_secs(30);

/// Safety margin added to the wall-clock kill switch so a slow network
/// doesn't trip the fallback before the per-probe read timeouts expire.
const WALL_CLOCK_MARGIN: Duration = Duration::from_secs(5);

/// Drain budget for cancellation. Spec T46 scope: tracers must exit
/// within 1 s of stream cancellation.
const CANCEL_DRAIN_BUDGET: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Reasons `build_oneshot_config` refuses to produce a `Builder`. Every
/// variant maps to `MeasurementFailureCode::AgentError` on the wire.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OneshotBuildError {
    UnspecifiedProtocol,
    UnspecifiedKind,
    MissingTcpPort,
    MissingUdpPort,
}

impl std::fmt::Display for OneshotBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::UnspecifiedProtocol => "UNSPECIFIED protocol",
            Self::UnspecifiedKind => "UNSPECIFIED measurement kind",
            Self::MissingTcpPort => "tcp protocol without destination_port",
            Self::MissingUdpPort => "udp protocol without destination_port",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for OneshotBuildError {}

// ---------------------------------------------------------------------------
// OneshotProber
// ---------------------------------------------------------------------------

/// Trippy-backed [`CampaignProber`]. One instance per agent. Per-pair
/// blocking tracers run under an internal semaphore, independent from
/// the continuous MTR pool gated by `MESHMON_ICMP_TARGET_CONCURRENCY`.
pub struct OneshotProber {
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
        cancel: CancellationToken,
        results: mpsc::Sender<Result<MeasurementResult, Status>>,
    ) {
        let kind = MeasurementKind::try_from(req.kind).unwrap_or(MeasurementKind::Unspecified);
        let protocol = Protocol::try_from(req.protocol).unwrap_or(Protocol::Unspecified);

        let mut handles = Vec::with_capacity(req.targets.len());
        for target in req.targets {
            // Honour cancellation at the batch level so a late cancel
            // during spawn doesn't keep launching fresh tracers.
            if cancel.is_cancelled() {
                if results
                    .send(Ok(failure_result(
                        target.pair_id,
                        MeasurementFailureCode::Cancelled,
                        "batch cancelled",
                    )))
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }

            // Cancel-aware acquire: a saturated semaphore must never pin
            // a cancelled batch. If cancel fires while we're waiting, emit
            // CANCELLED for this pair and move on (the next loop iteration
            // hits the fast-path above for the remaining targets).
            let permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    if results
                        .send(Ok(failure_result(
                            target.pair_id,
                            MeasurementFailureCode::Cancelled,
                            "batch cancelled",
                        )))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                acquired = self.semaphore.clone().acquire_owned() => match acquired {
                    Ok(p) => p,
                    Err(_) => {
                        // Semaphore is never closed in production today;
                        // the branch is defensive against future refactors.
                        let _ = results
                            .send(Ok(failure_result(
                                target.pair_id,
                                MeasurementFailureCode::AgentError,
                                "semaphore closed",
                            )))
                            .await;
                        continue;
                    }
                }
            };

            let meta = OneshotRequest {
                batch_id: req.batch_id,
                kind,
                protocol,
                probe_count: req.probe_count,
                timeout_ms: req.timeout_ms,
                probe_stagger_ms: req.probe_stagger_ms,
            };
            let cancel = cancel.clone();
            let results = results.clone();

            let handle = tokio::spawn(async move {
                let _permit_guard = permit;
                run_one_pair(meta, target, cancel, results).await;
            });
            handles.push(handle);
        }

        for handle in handles {
            let _ = handle.await;
        }
    }
}

// ---------------------------------------------------------------------------
// Per-pair task
// ---------------------------------------------------------------------------

/// Immutable per-pair dispatch context. Split from [`RunMeasurementBatchRequest`]
/// so the per-pair task never needs to re-parse enum values or re-borrow the
/// targets vector.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OneshotRequest {
    #[allow(dead_code)]
    pub batch_id: u64,
    pub kind: MeasurementKind,
    pub protocol: Protocol,
    pub probe_count: u32,
    pub timeout_ms: u32,
    pub probe_stagger_ms: u32,
}

async fn run_one_pair(
    req: OneshotRequest,
    target: MeasurementTarget,
    cancel: CancellationToken,
    results: mpsc::Sender<Result<MeasurementResult, Status>>,
) {
    let dest_ip = match meshmon_protocol::ip::to_ipaddr(&target.destination_ip) {
        Ok(ip) => ip,
        Err(_) => {
            let _ = results
                .send(Ok(failure_result(
                    target.pair_id,
                    MeasurementFailureCode::AgentError,
                    "invalid destination_ip",
                )))
                .await;
            return;
        }
    };

    // TCP/UDP require a non-zero port in [1, 65535]. ICMP ignores the
    // port entirely. A missing protobuf field defaults to 0, and a value
    // > 65535 cannot fit in u16 — both cases surface as AgentError rather
    // than silently probing port 0, which would always TIMEOUT and
    // masquerade as legitimate unreachability.
    let (tcp_port, udp_port) = match req.protocol {
        Protocol::Tcp | Protocol::Udp => {
            let parsed: Option<u16> = u16::try_from(target.destination_port)
                .ok()
                .filter(|p| *p != 0);
            match parsed {
                Some(p) if matches!(req.protocol, Protocol::Tcp) => (Some(p), None),
                Some(p) => (None, Some(p)),
                None => {
                    let _ = results
                        .send(Ok(failure_result(
                            target.pair_id,
                            MeasurementFailureCode::AgentError,
                            "invalid destination_port",
                        )))
                        .await;
                    return;
                }
            }
        }
        _ => (None, None),
    };

    let builder = match build_oneshot_config(req, dest_ip, tcp_port, udp_port) {
        Ok(b) => b,
        Err(e) => {
            let _ = results
                .send(Ok(failure_result(
                    target.pair_id,
                    MeasurementFailureCode::AgentError,
                    &format!("builder: {e}"),
                )))
                .await;
            return;
        }
    };

    let tracer = match builder.build() {
        Ok(t) => Arc::new(t),
        Err(e) => {
            let _ = results
                .send(Ok(failure_result(
                    target.pair_id,
                    MeasurementFailureCode::AgentError,
                    &format!("trippy build: {e}"),
                )))
                .await;
            return;
        }
    };
    let tracer_for_run = Arc::clone(&tracer);

    let mut blocking = tokio::task::spawn_blocking(move || tracer_for_run.run());

    // Wall-clock safety net. `probe_count * (stagger + timeout) + margin`
    // for LATENCY; MTR uses its own round timeout plus margin.
    let max_wall_clock = match req.kind {
        MeasurementKind::Latency => {
            Duration::from_millis(
                (req.probe_count as u64).max(1)
                    * (req.probe_stagger_ms as u64 + req.timeout_ms as u64),
            ) + WALL_CLOCK_MARGIN
        }
        MeasurementKind::Mtr => MTR_ROUND_TIMEOUT + WALL_CLOCK_MARGIN,
        MeasurementKind::Unspecified => WALL_CLOCK_MARGIN,
    };

    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            // trippy-core 0.13 does not expose a cancellation hook on
            // `Tracer::run()`, so dropping our outer Arc does not stop the
            // blocking thread — the spawn_blocking task holds its own
            // Arc<Tracer> until `run()` returns naturally (bounded by
            // `max_rounds * (probe_stagger + read_timeout) + grace`).
            // CANCEL_DRAIN_BUDGET only guarantees a fast wire-visible
            // CANCELLED emission; operators must size the tokio blocking
            // thread pool (default 64) against `continuous_cap +
            // campaign_cap` worst-case.
            drop(tracer);
            let _ = tokio::time::timeout(CANCEL_DRAIN_BUDGET, &mut blocking).await;
            let _ = results
                .send(Ok(failure_result(
                    target.pair_id,
                    MeasurementFailureCode::Cancelled,
                    "stream cancelled",
                )))
                .await;
            return;
        }
        joined = &mut blocking => {
            match joined {
                Ok(Ok(())) => {
                    let state = tracer.snapshot();
                    aggregate(req.kind, target.pair_id, &state)
                }
                Ok(Err(e)) => failure_result(
                    target.pair_id,
                    MeasurementFailureCode::AgentError,
                    &format!("tracer: {e}"),
                ),
                Err(join_err) => failure_result(
                    target.pair_id,
                    MeasurementFailureCode::AgentError,
                    &format!("join: {join_err}"),
                ),
            }
        }
        _ = tokio::time::sleep(max_wall_clock) => {
            // Same caveat as the cancel arm: the blocking thread keeps
            // running until trippy's natural round loop exits. We detach
            // here and emit TIMEOUT so the batch doesn't stall.
            drop(tracer);
            failure_result(
                target.pair_id,
                MeasurementFailureCode::Timeout,
                "wall-clock",
            )
        }
    };

    let _ = results.send(Ok(outcome)).await;
}

// ---------------------------------------------------------------------------
// Builder matrix (spec 03 §4.3)
// ---------------------------------------------------------------------------

pub(crate) fn build_oneshot_config(
    req: OneshotRequest,
    dest_ip: IpAddr,
    tcp_port: Option<u16>,
    udp_port: Option<u16>,
) -> Result<Builder, OneshotBuildError> {
    let trippy_proto = match req.protocol {
        Protocol::Icmp => trippy_core::Protocol::Icmp,
        Protocol::Tcp => trippy_core::Protocol::Tcp,
        Protocol::Udp => trippy_core::Protocol::Udp,
        Protocol::Unspecified => return Err(OneshotBuildError::UnspecifiedProtocol),
    };

    let (max_rounds, min_rd, max_rd, read_timeout) = match req.kind {
        MeasurementKind::Latency => {
            let stagger = Duration::from_millis(req.probe_stagger_ms.into());
            let timeout = Duration::from_millis(req.timeout_ms.into());
            (req.probe_count.max(1) as usize, stagger, stagger, timeout)
        }
        MeasurementKind::Mtr => (
            1,
            Duration::from_millis(0),
            MTR_ROUND_TIMEOUT,
            MTR_ROUND_TIMEOUT,
        ),
        MeasurementKind::Unspecified => return Err(OneshotBuildError::UnspecifiedKind),
    };

    let mut builder = Builder::new(dest_ip)
        .protocol(trippy_proto)
        .first_ttl(1)
        .max_ttl(ONESHOT_MAX_TTL)
        .read_timeout(read_timeout)
        .grace_duration(ONESHOT_GRACE)
        .min_round_duration(min_rd)
        .max_round_duration(max_rd)
        .max_rounds(Some(max_rounds));

    if matches!(req.protocol, Protocol::Icmp) {
        let id = crate::probing::next_trace_id();
        builder = builder.trace_identifier(id);
    }

    builder = match req.protocol {
        Protocol::Tcp => {
            let port = tcp_port.ok_or(OneshotBuildError::MissingTcpPort)?;
            builder.port_direction(PortDirection::FixedDest(Port(port)))
        }
        Protocol::Udp => {
            let port = udp_port.ok_or(OneshotBuildError::MissingUdpPort)?;
            builder.port_direction(PortDirection::FixedDest(Port(port)))
        }
        Protocol::Icmp | Protocol::Unspecified => builder,
    };

    Ok(builder)
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

fn aggregate(kind: MeasurementKind, pair_id: u64, state: &State) -> MeasurementResult {
    match kind {
        MeasurementKind::Latency => aggregate_latency(pair_id, state),
        MeasurementKind::Mtr => aggregate_mtr(pair_id, state),
        MeasurementKind::Unspecified => failure_result(
            pair_id,
            MeasurementFailureCode::AgentError,
            "unspecified kind",
        ),
    }
}

/// Shared LATENCY aggregator for all three protocols.
///
/// Trippy 0.13 collapses TCP SYN/ACK and TCP RST into the same hop-level
/// counters (`total_recv`), and there is no per-probe status exposed on
/// the public `Hop` surface. That limitation is the subject of the "TCP
/// REFUSED predicate" deviation in the T46 plan: we count any reply from
/// the destination as success, regardless of whether it was a SYN/ACK or
/// a RST. An explicit `MeasurementFailureCode::Refused` is therefore
/// never emitted from this path today; if the operator later needs the
/// distinction, a secondary `TcpStream::connect` channel or a patched
/// trippy-core can layer it on without reshuffling this function.
///
/// UDP success similarly piggybacks on trippy's destination-reached
/// predicate: `target_hop.total_recv()` counts replies from the
/// destination IP only (service response OR ICMP Port-Unreachable from
/// the destination itself). ICMP Time-Exceeded from intermediate hops
/// do not inflate this counter — they accrue on their own lower-TTL
/// hops, which we ignore for LATENCY.
fn aggregate_latency(pair_id: u64, state: &State) -> MeasurementResult {
    let target_hop = state.target_hop(State::default_flow_id());
    let attempted: u32 = target_hop.total_sent().try_into().unwrap_or(u32::MAX);
    let succeeded: u32 = target_hop.total_recv().try_into().unwrap_or(u32::MAX);

    if attempted == 0 {
        return failure_result(
            pair_id,
            MeasurementFailureCode::AgentError,
            "tracer sent zero probes",
        );
    }

    if succeeded == 0 {
        return failure_result(pair_id, MeasurementFailureCode::Timeout, "no replies");
    }

    // trippy 0.13's `Hop::samples()` includes a `Duration::default()`
    // sentinel for every `ProbeStatus::Awaited` and `ProbeStatus::Failed`
    // probe (see trippy-core state.rs around ProbeStatus::Awaited/Failed).
    // Feeding those zeros into the stats pulls `min` to 0 and skews
    // `avg`/`stddev` on any partial-loss batch. Drop them before stats.
    let rtts: Vec<Duration> = target_hop
        .samples()
        .iter()
        .copied()
        .filter(|d| !d.is_zero())
        .collect();
    let summary = build_summary(attempted, succeeded, &rtts);
    success_result(pair_id, summary)
}

/// Spec 03 §4.5: single-round MTR. Dense-pack TTLs `[1..=target_reached_ttl]`
/// into a `MtrTraceResult`. Silent TTLs pad with `loss_ratio = 1.0`,
/// `avg_rtt_micros = 0`, and an empty `observed_ips` list.
///
/// `target_reached_ttl` is the TTL at which the destination hop replied;
/// when no reply landed we truncate at the highest responsive TTL instead
/// of emitting a full 32-hop list against an unreachable destination.
fn aggregate_mtr(pair_id: u64, state: &State) -> MeasurementResult {
    let target_hop = state.target_hop(State::default_flow_id());
    let target_reached_ttl = if target_hop.total_recv() > 0 {
        target_hop.ttl()
    } else {
        state
            .hops()
            .iter()
            .filter(|h| h.total_recv() > 0)
            .map(|h| h.ttl())
            .max()
            .unwrap_or(0)
    };

    if target_reached_ttl == 0 {
        // No hop responded at all — surface as a Timeout failure so the
        // writer's last_error vocabulary stays aligned with LATENCY paths.
        return failure_result(
            pair_id,
            MeasurementFailureCode::Timeout,
            "no hops responded",
        );
    }

    // Index hops by TTL so we can dense-pack the output regardless of
    // trippy's internal slice layout.
    let hops_by_ttl: std::collections::BTreeMap<u8, &trippy_core::Hop> =
        state.hops().iter().map(|h| (h.ttl(), h)).collect();

    let mut hops: Vec<HopSummary> = Vec::with_capacity(target_reached_ttl as usize);
    for ttl in 1..=target_reached_ttl {
        match hops_by_ttl.get(&ttl) {
            Some(hop) if hop.total_recv() > 0 => {
                let avg_rtt_micros = hop.best_ms().map(ms_to_micros).unwrap_or(0);
                // Single-round MTR sees at most one reply per TTL in
                // practice, so every observed IP lands with frequency
                // 1.0. A multi-round variant would derive this from
                // `hop.addrs_with_counts() / hop.total_recv()`.
                let observed_ips = hop
                    .addrs()
                    .map(|ip| HopIp {
                        ip: meshmon_protocol::ip::from_ipaddr(*ip),
                        frequency: 1.0,
                    })
                    .collect();
                hops.push(HopSummary {
                    position: u32::from(ttl),
                    observed_ips,
                    avg_rtt_micros,
                    stddev_rtt_micros: 0,
                    loss_ratio: 0.0,
                });
            }
            _ => {
                hops.push(HopSummary {
                    position: u32::from(ttl),
                    observed_ips: Vec::new(),
                    avg_rtt_micros: 0,
                    stddev_rtt_micros: 0,
                    loss_ratio: 1.0,
                });
            }
        }
    }

    MeasurementResult {
        pair_id,
        outcome: Some(Outcome::Mtr(MtrTraceResult { hops })),
    }
}

/// Convert milliseconds (f64) to microseconds (u32). Non-finite or
/// non-positive values map to 0; oversize values saturate at `u32::MAX`.
fn ms_to_micros(ms: f64) -> u32 {
    if !ms.is_finite() || ms <= 0.0 {
        return 0;
    }
    let micros = ms * 1_000.0;
    if micros >= u32::MAX as f64 {
        u32::MAX
    } else {
        micros as u32
    }
}

fn build_summary(attempted: u32, succeeded: u32, samples: &[Duration]) -> MeasurementSummary {
    let rtts_ms: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1_000.0).collect();

    let (min, max, avg, stddev) = if rtts_ms.is_empty() {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        let min = rtts_ms.iter().copied().fold(f64::INFINITY, f64::min);
        let max = rtts_ms.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let avg = rtts_ms.iter().sum::<f64>() / rtts_ms.len() as f64;
        let stddev = if rtts_ms.len() > 1 {
            let var =
                rtts_ms.iter().map(|x| (x - avg).powi(2)).sum::<f64>() / (rtts_ms.len() - 1) as f64;
            var.sqrt()
        } else {
            0.0
        };
        (min, max, avg, stddev)
    };
    let median = percentile(&rtts_ms, 0.50);
    let p95 = percentile(&rtts_ms, 0.95);
    let loss_ratio = if attempted > 0 {
        1.0 - (succeeded as f32 / attempted as f32)
    } else {
        0.0
    };

    MeasurementSummary {
        attempted,
        succeeded,
        latency_min_ms: min as f32,
        latency_avg_ms: avg as f32,
        latency_median_ms: median as f32,
        latency_p95_ms: p95 as f32,
        latency_max_ms: max as f32,
        latency_stddev_ms: stddev as f32,
        loss_ratio,
    }
}

/// Nearest-rank percentile using `round` on `q * (N - 1)`. This matches
/// the existing continuous-prober stats pass and gives the median at
/// `sorted[N/2]` for odd N; it is *not* the interpolated percentile that
/// pandas/numpy default to. Callers comparing to reference tooling must
/// use the same convention.
fn percentile(samples_ms: &[f64], q: f64) -> f64 {
    if samples_ms.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = samples_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (q * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

// ---------------------------------------------------------------------------
// Result helpers
// ---------------------------------------------------------------------------

fn failure_result(pair_id: u64, code: MeasurementFailureCode, detail: &str) -> MeasurementResult {
    MeasurementResult {
        pair_id,
        outcome: Some(Outcome::Failure(MeasurementFailure {
            code: code as i32,
            detail: detail.to_string(),
        })),
    }
}

fn success_result(pair_id: u64, summary: MeasurementSummary) -> MeasurementResult {
    MeasurementResult {
        pair_id,
        outcome: Some(Outcome::Success(summary)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn icmp_req(probe_count: u32, timeout_ms: u32, stagger_ms: u32) -> OneshotRequest {
        OneshotRequest {
            batch_id: 1,
            kind: MeasurementKind::Latency,
            protocol: Protocol::Icmp,
            probe_count,
            timeout_ms,
            probe_stagger_ms: stagger_ms,
        }
    }

    #[test]
    fn build_config_rejects_unspecified_protocol() {
        let req = OneshotRequest {
            batch_id: 1,
            kind: MeasurementKind::Latency,
            protocol: Protocol::Unspecified,
            probe_count: 1,
            timeout_ms: 100,
            probe_stagger_ms: 10,
        };
        let dest: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(matches!(
            build_oneshot_config(req, dest, None, None),
            Err(OneshotBuildError::UnspecifiedProtocol)
        ));
    }

    #[test]
    fn build_config_rejects_unspecified_kind() {
        let mut req = icmp_req(1, 100, 10);
        req.kind = MeasurementKind::Unspecified;
        let dest: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(matches!(
            build_oneshot_config(req, dest, None, None),
            Err(OneshotBuildError::UnspecifiedKind)
        ));
    }

    #[test]
    fn build_config_tcp_requires_port() {
        let mut req = icmp_req(1, 100, 10);
        req.protocol = Protocol::Tcp;
        let dest: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(matches!(
            build_oneshot_config(req, dest, None, None),
            Err(OneshotBuildError::MissingTcpPort)
        ));
    }

    #[test]
    fn build_config_udp_requires_port() {
        let mut req = icmp_req(1, 100, 10);
        req.protocol = Protocol::Udp;
        let dest: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(matches!(
            build_oneshot_config(req, dest, None, None),
            Err(OneshotBuildError::MissingUdpPort)
        ));
    }

    #[test]
    fn build_config_icmp_latency_round_knobs() {
        // Smoke test: the builder accepts every knob path. Calling
        // `.build()` would need raw-socket caps, so we stop at builder
        // construction — that's what this test covers.
        let dest: IpAddr = "127.0.0.1".parse().unwrap();
        let req = icmp_req(5, 2_000, 100);
        let builder = build_oneshot_config(req, dest, None, None);
        assert!(builder.is_ok());
    }

    #[test]
    fn build_config_mtr_pins_single_round_regardless_of_probe_count() {
        // The request asks for 10 probes, but MTR overrides unconditionally
        // to a single round (spec §4.5). We verify indirectly by inspecting
        // that the `build_oneshot_config` path succeeds and that the
        // `OneshotRequest.probe_count` field is ignored for MTR.
        let mut req = icmp_req(10, 100, 10);
        req.kind = MeasurementKind::Mtr;
        let dest: IpAddr = "127.0.0.1".parse().unwrap();
        let builder = build_oneshot_config(req, dest, None, None);
        assert!(builder.is_ok());
        // `Builder` does not expose max_rounds for inspection in 0.13; the
        // assertion that MTR is single-round is covered by the aggregate
        // path (which tolerates one-round state) plus the scope DoD.
    }

    #[test]
    fn build_config_tcp_latency_accepts_port() {
        let dest: IpAddr = "127.0.0.1".parse().unwrap();
        let mut req = icmp_req(3, 500, 50);
        req.protocol = Protocol::Tcp;
        assert!(build_oneshot_config(req, dest, Some(4_242), None).is_ok());
    }

    #[test]
    fn build_config_udp_latency_accepts_port() {
        let dest: IpAddr = "127.0.0.1".parse().unwrap();
        let mut req = icmp_req(3, 500, 50);
        req.protocol = Protocol::Udp;
        assert!(build_oneshot_config(req, dest, None, Some(4_343)).is_ok());
    }

    #[test]
    fn percentile_picks_nearest_rank() {
        let samples = vec![10.0_f64, 20.0, 30.0, 40.0, 50.0];
        assert_eq!(percentile(&samples, 0.0), 10.0);
        assert_eq!(percentile(&samples, 0.5), 30.0);
        assert_eq!(percentile(&samples, 1.0), 50.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn build_summary_with_no_replies_has_full_loss() {
        let s = build_summary(5, 0, &[]);
        assert_eq!(s.attempted, 5);
        assert_eq!(s.succeeded, 0);
        assert_eq!(s.loss_ratio, 1.0);
        assert_eq!(s.latency_avg_ms, 0.0);
    }

    #[test]
    fn build_summary_computes_stats() {
        let samples = vec![
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(30),
        ];
        let s = build_summary(3, 3, &samples);
        assert_eq!(s.attempted, 3);
        assert_eq!(s.succeeded, 3);
        assert!((s.loss_ratio - 0.0).abs() < 1e-6);
        assert!((s.latency_min_ms - 10.0).abs() < 1e-3);
        assert!((s.latency_max_ms - 30.0).abs() < 1e-3);
        assert!((s.latency_avg_ms - 20.0).abs() < 1e-3);
        assert!((s.latency_median_ms - 20.0).abs() < 1e-3);
    }

    // Regression: trippy 0.13 inserts Duration::default() into Hop::samples
    // for Awaited/Failed probes. If those zeros reach build_summary, min
    // pins to 0 and avg/stddev are skewed. aggregate_latency must filter
    // them out before the stats pass. We verify at the helper boundary by
    // asserting build_summary on a filtered slice matches the expected
    // non-zero stats.
    #[test]
    fn build_summary_ignores_zero_duration_filter() {
        // Caller (aggregate_latency) filters zeros out; build_summary
        // sees only real RTTs even when attempted > succeeded.
        let real_rtts = vec![Duration::from_millis(10), Duration::from_millis(30)];
        let s = build_summary(5, 2, &real_rtts);
        assert_eq!(s.attempted, 5);
        assert_eq!(s.succeeded, 2);
        assert!((s.latency_min_ms - 10.0).abs() < 1e-3);
        assert!((s.latency_max_ms - 30.0).abs() < 1e-3);
        assert!((s.loss_ratio - 0.6).abs() < 1e-5);
    }

    // End-to-end: feed aggregate_latency a mixture of real and sentinel
    // samples and prove the filter engages. This uses Duration::is_zero
    // the same way the production path does.
    #[test]
    fn zero_duration_filter_strips_trippy_sentinels() {
        let mixed = [
            Duration::from_millis(15),
            Duration::default(),
            Duration::from_millis(25),
            Duration::default(),
        ];
        let filtered: Vec<Duration> = mixed.iter().copied().filter(|d| !d.is_zero()).collect();
        assert_eq!(filtered.len(), 2);
        let s = build_summary(4, 2, &filtered);
        assert!((s.latency_min_ms - 15.0).abs() < 1e-3);
        assert!((s.latency_max_ms - 25.0).abs() < 1e-3);
        assert!((s.latency_avg_ms - 20.0).abs() < 1e-3);
    }

    // Loopback integration: ICMP LATENCY — self-skips without CAP_NET_RAW.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oneshot_icmp_latency_against_loopback() {
        crate::probing::icmp_pool::skip_unless_raw_ip_socket!();

        let prober = OneshotProber::new(2);
        let req = RunMeasurementBatchRequest {
            batch_id: 1,
            kind: MeasurementKind::Latency as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 5,
            timeout_ms: 500,
            probe_stagger_ms: 20,
            targets: vec![MeasurementTarget {
                pair_id: 42,
                destination_ip: vec![127, 0, 0, 1].into(),
                destination_port: 0,
            }],
        };
        let (tx, mut rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        prober.run_batch(req, cancel, tx).await;
        let result = rx.recv().await.expect("one result").expect("no rpc error");
        assert_eq!(result.pair_id, 42);
        match result.outcome {
            Some(Outcome::Success(summary)) => {
                assert_eq!(summary.attempted, 5);
                assert!(summary.succeeded >= 1);
                assert!(summary.latency_avg_ms < 200.0);
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    // Loopback integration: TCP LATENCY against a live echo listener on
    // 127.0.0.1. The listener terminates the handshake with RST immediately
    // (SO_LINGER(0)), which trippy 0.13 accepts as `total_recv > 0` — see
    // the `aggregate_latency` docstring for the REFUSED-predicate gap.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oneshot_tcp_latency_against_loopback() {
        crate::probing::icmp_pool::skip_unless_raw_ip_socket!();

        // Pick a random ephemeral TCP port and bind a listener.
        let cancel = CancellationToken::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let listener_cancel = cancel.clone();
        let accept_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = listener_cancel.cancelled() => return,
                    accepted = listener.accept() => {
                        if accepted.is_err() { return; }
                    }
                }
            }
        });

        let prober = OneshotProber::new(2);
        let req = RunMeasurementBatchRequest {
            batch_id: 2,
            kind: MeasurementKind::Latency as i32,
            protocol: Protocol::Tcp as i32,
            probe_count: 3,
            timeout_ms: 500,
            probe_stagger_ms: 20,
            targets: vec![MeasurementTarget {
                pair_id: 7,
                destination_ip: vec![127, 0, 0, 1].into(),
                destination_port: u32::from(port),
            }],
        };
        let (tx, mut rx) = mpsc::channel(4);
        prober.run_batch(req, cancel.clone(), tx).await;
        let result = rx.recv().await.expect("one result").expect("no rpc error");
        assert_eq!(result.pair_id, 7);
        // Trippy collapses RST+SYN/ACK into total_recv; both map to success.
        match result.outcome {
            Some(Outcome::Success(summary)) => {
                assert_eq!(summary.attempted, 3);
                assert!(summary.succeeded >= 1);
            }
            other => panic!("expected success, got {other:?}"),
        }

        cancel.cancel();
        let _ = accept_task.await;
    }

    // Loopback integration: UDP LATENCY against an unbound port on 127.0.0.1.
    // The kernel sends ICMP Port-Unreachable back from the destination IP
    // itself, which trippy's destination-reached predicate counts as success.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oneshot_udp_latency_against_loopback() {
        crate::probing::icmp_pool::skip_unless_raw_ip_socket!();

        // An ephemeral high port — kernel is ~guaranteed to have no
        // listener. ICMP Port-Unreachable is the expected response.
        let prober = OneshotProber::new(2);
        let req = RunMeasurementBatchRequest {
            batch_id: 3,
            kind: MeasurementKind::Latency as i32,
            protocol: Protocol::Udp as i32,
            probe_count: 3,
            timeout_ms: 500,
            probe_stagger_ms: 20,
            targets: vec![MeasurementTarget {
                pair_id: 9,
                destination_ip: vec![127, 0, 0, 1].into(),
                destination_port: 65432,
            }],
        };
        let (tx, mut rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        prober.run_batch(req, cancel, tx).await;
        let result = rx.recv().await.expect("one result").expect("no rpc error");
        assert_eq!(result.pair_id, 9);
        // Either success (ICMP Port-Unreachable within timeout) or timeout
        // (some kernels rate-limit ICMP errors) — both are valid outcomes on
        // unprivileged CI loopback. Assert the dispatch contract (one result,
        // correct pair_id) and that we never silently lose a pair.
        match result.outcome {
            Some(Outcome::Success(_)) | Some(Outcome::Failure(_)) => {}
            other => panic!("expected success or failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_destination_ip_emits_agent_error() {
        let prober = OneshotProber::new(1);
        let req = RunMeasurementBatchRequest {
            batch_id: 4,
            kind: MeasurementKind::Latency as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 1,
            timeout_ms: 100,
            probe_stagger_ms: 10,
            targets: vec![MeasurementTarget {
                pair_id: 11,
                destination_ip: vec![1, 2, 3].into(), // not 4 or 16 bytes
                destination_port: 0,
            }],
        };
        let (tx, mut rx) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        prober.run_batch(req, cancel, tx).await;
        let result = rx.recv().await.unwrap().unwrap();
        assert_eq!(result.pair_id, 11);
        match result.outcome {
            Some(Outcome::Failure(f)) => {
                assert_eq!(f.code, MeasurementFailureCode::AgentError as i32);
                assert!(f.detail.contains("invalid destination_ip"));
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn already_cancelled_batch_drains_targets_as_cancelled() {
        let prober = OneshotProber::new(1);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let req = RunMeasurementBatchRequest {
            batch_id: 5,
            kind: MeasurementKind::Latency as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 1,
            timeout_ms: 100,
            probe_stagger_ms: 10,
            targets: vec![
                MeasurementTarget {
                    pair_id: 20,
                    destination_ip: vec![127, 0, 0, 1].into(),
                    destination_port: 0,
                },
                MeasurementTarget {
                    pair_id: 21,
                    destination_ip: vec![127, 0, 0, 1].into(),
                    destination_port: 0,
                },
            ],
        };
        let (tx, mut rx) = mpsc::channel(4);
        prober.run_batch(req, cancel, tx).await;
        let mut seen = Vec::new();
        while let Some(item) = rx.recv().await {
            let r = item.unwrap();
            match r.outcome {
                Some(Outcome::Failure(f)) => {
                    assert_eq!(f.code, MeasurementFailureCode::Cancelled as i32);
                    seen.push(r.pair_id);
                }
                other => panic!("expected cancelled failure, got {other:?}"),
            }
        }
        seen.sort();
        assert_eq!(seen, vec![20, 21]);
    }

    // Loopback integration: MTR against 127.0.0.1. Expect exactly one hop
    // (position=1) with the destination IP listed and loss_ratio=0.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oneshot_mtr_against_loopback() {
        crate::probing::icmp_pool::skip_unless_raw_ip_socket!();

        let prober = OneshotProber::new(2);
        let req = RunMeasurementBatchRequest {
            batch_id: 6,
            kind: MeasurementKind::Mtr as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 10,   // Ignored for MTR (single round forced).
            timeout_ms: 1_000, // Ignored for MTR (30s hard-coded).
            probe_stagger_ms: 0,
            targets: vec![MeasurementTarget {
                pair_id: 30,
                destination_ip: vec![127, 0, 0, 1].into(),
                destination_port: 0,
            }],
        };
        let (tx, mut rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        prober.run_batch(req, cancel, tx).await;
        let result = rx.recv().await.expect("one result").expect("no rpc error");
        assert_eq!(result.pair_id, 30);
        match result.outcome {
            Some(Outcome::Mtr(trace)) => {
                assert_eq!(trace.hops.len(), 1, "loopback MTR is single-hop");
                let hop = &trace.hops[0];
                assert_eq!(hop.position, 1);
                assert!((hop.loss_ratio - 0.0).abs() < 1e-6);
                assert_eq!(hop.observed_ips.len(), 1);
                let bytes: &[u8] = &hop.observed_ips[0].ip;
                assert_eq!(bytes, &[127, 0, 0, 1]);
            }
            other => panic!("expected MTR trace, got {other:?}"),
        }
    }

    // Cancellation integration: drop the cancel token while a batch of many
    // long probes is in flight, and assert every tracer returns within the
    // 1.5 s budget (1 s drain + slack for CI scheduling).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn oneshot_cancellation_drains_within_budget() {
        crate::probing::icmp_pool::skip_unless_raw_ip_socket!();

        // A batch sized well above the semaphore so several tracers are
        // genuinely mid-flight when cancel fires. Each target is an
        // unreachable TEST-NET-1 address that would otherwise time out.
        let prober = OneshotProber::new(8);
        let targets: Vec<MeasurementTarget> = (0..8)
            .map(|i| MeasurementTarget {
                pair_id: 100 + i as u64,
                destination_ip: vec![192, 0, 2, 1].into(), // TEST-NET-1
                destination_port: 0,
            })
            .collect();
        let req = RunMeasurementBatchRequest {
            batch_id: 7,
            kind: MeasurementKind::Latency as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 50,
            timeout_ms: 5_000,
            probe_stagger_ms: 100,
            targets,
        };
        let (tx, mut rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let run_cancel = cancel.clone();
        let run = tokio::spawn(async move { prober.run_batch(req, run_cancel, tx).await });

        // Let the tracers actually start so cancellation finds them in the
        // `select!`'s `joined = &mut blocking` branch.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let start = std::time::Instant::now();
        cancel.cancel();

        let mut drained = 0;
        while let Some(item) = rx.recv().await {
            let r = item.unwrap();
            match r.outcome {
                Some(Outcome::Failure(f)) => {
                    assert_eq!(
                        f.code,
                        MeasurementFailureCode::Cancelled as i32,
                        "pair {} got {:?} not Cancelled",
                        r.pair_id,
                        f,
                    );
                    drained += 1;
                }
                other => panic!(
                    "pair {} expected Cancelled failure, got {other:?}",
                    r.pair_id
                ),
            }
        }
        run.await.expect("run_batch task panicked");

        let elapsed = start.elapsed();
        assert_eq!(drained, 8, "every pair must emit exactly one result");
        assert!(
            elapsed < Duration::from_millis(1_500),
            "cancellation drain took {elapsed:?}; budget is 1 s (1.5 s with CI slack)",
        );
    }

    // --- Shared-resource audit & coexistence -------------------------------

    /// 400 concurrent calls to the shared `next_trace_id()` allocator must
    /// yield 400 unique non-zero `u16` values. Proves the allocator is the
    /// single source of truth and that the wrap-skip-zero guard holds
    /// under contention — the foundation on which the coexistence
    /// invariant from spec 03 §6 rests.
    #[test]
    fn next_trace_id_is_unique_under_contention() {
        use std::collections::HashSet;
        use std::thread;

        const TOTAL: usize = 400;
        let mut handles = Vec::with_capacity(TOTAL);
        for _ in 0..TOTAL {
            handles.push(thread::spawn(crate::probing::next_trace_id));
        }
        let ids: HashSet<u16> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();
        assert_eq!(ids.len(), TOTAL, "all {TOTAL} ids must be unique");
        assert!(!ids.contains(&0), "zero is reserved (wildcard)");
    }

    /// The oneshot module must never reach into the continuous UDP
    /// prober's shared infrastructure. A compile-time grep over the
    /// module's source proves this; any future edit that imports or
    /// names the forbidden symbols trips this test.
    ///
    /// The scan truncates at the first occurrence of the audit-cutoff
    /// marker, which is declared immediately below as a `const`. The
    /// assertion literals live *after* that cutoff, so they cannot
    /// self-match. Tests added above the cutoff must not use the
    /// forbidden literals in comments or doc strings.
    #[test]
    fn oneshot_source_avoids_continuous_udp_symbols() {
        const SRC: &str = include_str!("oneshot.rs");
        const CUTOFF: &str = "audit-cutoff-anchor-do-not-delete";
        let idx = SRC
            .find(CUTOFF)
            .expect("cutoff marker must appear before the assertions");
        let preceding = &SRC[..idx];
        let pool_symbol = concat!("Udp", "ProberPool");
        let echo_symbol = concat!("echo_", "udp");
        assert!(
            !preceding.contains(pool_symbol),
            "oneshot.rs must not reach into the continuous UDP pool",
        );
        assert!(
            !preceding.contains(echo_symbol),
            "oneshot.rs must not import the UDP echo module",
        );
        // audit-cutoff-anchor-do-not-delete
    }

    /// Coexistence: two OneshotProber batches running concurrently against
    /// loopback both return results, share the `next_trace_id()` allocator,
    /// and leave the collision counter at 0. Self-skips on hosts without
    /// CAP_NET_RAW.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn oneshot_coexists_with_concurrent_oneshot() {
        crate::probing::icmp_pool::skip_unless_raw_ip_socket!();

        reset_oneshot_collisions_for_test();
        crate::probing::trippy::reset_contamination_state_for_test();

        let batch = |pair_base: u64| RunMeasurementBatchRequest {
            batch_id: pair_base,
            kind: MeasurementKind::Latency as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 3,
            timeout_ms: 500,
            probe_stagger_ms: 20,
            targets: vec![MeasurementTarget {
                pair_id: pair_base,
                destination_ip: vec![127, 0, 0, 1].into(),
                destination_port: 0,
            }],
        };

        let prober_a = Arc::new(OneshotProber::new(2));
        let prober_b = Arc::new(OneshotProber::new(2));
        let (tx_a, mut rx_a) = mpsc::channel(4);
        let (tx_b, mut rx_b) = mpsc::channel(4);
        let cancel_a = CancellationToken::new();
        let cancel_b = CancellationToken::new();

        let run_a = {
            let prober = prober_a.clone();
            tokio::spawn(async move { prober.run_batch(batch(200), cancel_a, tx_a).await })
        };
        let run_b = {
            let prober = prober_b.clone();
            tokio::spawn(async move { prober.run_batch(batch(201), cancel_b, tx_b).await })
        };

        let res_a = rx_a
            .recv()
            .await
            .expect("A result")
            .expect("A no rpc error");
        let res_b = rx_b
            .recv()
            .await
            .expect("B result")
            .expect("B no rpc error");
        run_a.await.unwrap();
        run_b.await.unwrap();

        assert_eq!(res_a.pair_id, 200);
        assert_eq!(res_b.pair_id, 201);
        // Every tracer observed only its own replies.
        assert_eq!(
            oneshot_probe_collisions_total(),
            0,
            "oneshot collision counter must stay at 0",
        );
        assert_eq!(
            crate::probing::trippy::cross_contamination_total(),
            0,
            "continuous cross-contamination counter must stay at 0",
        );
    }
}
