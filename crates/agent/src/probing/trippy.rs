//! Trippy (MTR) prober.
//!
//! One persistent [`Tracer`] per (target, protocol, pps) tuple. The lifecycle:
//!
//! 1. Build one persistent `Tracer` per (target, protocol, pps) tuple.
//! 2. Hand it to `Tracer::run_with(callback)` inside a single `spawn_blocking` task.
//! 3. The callback snapshots state after each round and forwards aggregated hops via
//!    an async bridge to the per-target loop.
//! 4. The async loop runs contamination detection and emits `RouteTraceMsg` on the
//!    route-trace channel; failed rounds and panicked workers are log-only.
//! 5. On cancellation: drop the bridge sender; the callback's `blocking_send` errors;
//!    `run_with` returns; the active round completes before exit (worst-case ~10s).
//! 6. Protocol or pps change: rebuild the tracer (no live rate adjustment in trippy-core).
//!
//! Trippy emits topology data ([`RouteTraceMsg`]) into the supervisor's tracker
//! channel, never reachability samples ([`ProbeObservation`]). Reachability is
//! owned by the dedicated ICMP/TCP/UDP probers.
//!
//! ## Trace-identifier allocation
//!
//! Each ICMP round picks a unique non-zero `u16` via `next_trace_id()`, drawn
//! from a module-local `AtomicU16` (`NEXT_ICMP_TRACE_ID`) seeded randomly at
//! first use so a restarted process doesn't replay the same sequence against
//! stale in-flight replies. The counter wraps naturally; `0` is skipped on wrap
//! because trippy-core treats `TraceId(0)` as a wildcard that accepts any
//! matching reply — two concurrent ICMP tracers with `TraceId(0)` would
//! cross-attribute each other's replies. TCP/UDP rounds leave the default
//! trace identifier; trippy matches those on port/address, not ICMP identifier.
//!
//! ## Cross-contamination detection
//!
//! After each round, hops are checked against the peer-IP allowlist (a
//! `watch::Receiver<Arc<HashSet<IpAddr>>>` fed by `GetTargets`). If any hop
//! carries a peer IP that is not our own target's IP, the observation is
//! discarded and `CROSS_CONTAMINATION_TOTAL` is incremented. The allowlist
//! `borrow()` is scoped so the `watch::Ref` is released before any `.await`.
//!
//! ## Discard semantics
//!
//! Contaminated rounds are dropped silently — they are NOT emitted as
//! [`RouteTraceMsg`] (which would feed the route tracker with foreign path
//! data). A `tracing::warn!` fires at most once per 60 s per process
//! (rate-limited via `LAST_CONTAMINATION_WARN_NANOS`) and names the sibling
//! IP that leaked.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use meshmon_protocol::{Protocol, Target};
use tokio::sync::{mpsc, watch, Semaphore};
use tokio_util::sync::CancellationToken;
use trippy_core::{Builder, Port, PortDirection, State};

use crate::probing::{HopObservation, RouteTraceMsg, TrippyRate};

/// Maximum TTL (hops) the tracer will emit probes for.
const MAX_TTL: u8 = 30;

/// Per-round ICMP trace identifier allocator.
///
/// trippy-core 0.13 uses the ICMP `identifier` field to match replies to
/// the originating tracer (`strategy.rs::check_trace_id`). The default is
/// `TraceId(0)`, which any tracer also accepts as a fallback — so two
/// concurrent ICMP tracers both accept each other's replies, and foreign
/// targets' hops leak into unrelated paths. We allocate a unique non-zero
/// `u16` per round from this atomic.
///
/// The counter wraps naturally. We skip `0` on wrap because `TraceId(0)`
/// is the wildcard-accept fallback. The initial value is randomized at
/// first use so a restarted agent doesn't replay the same id sequence
/// against any stale replies still in flight from the previous process.
static NEXT_ICMP_TRACE_ID: LazyLock<AtomicU16> = LazyLock::new(|| {
    use rand::Rng;
    let seed = rand::rng().random_range(1..=u16::MAX);
    AtomicU16::new(seed)
});

fn next_trace_id() -> u16 {
    let mut id = NEXT_ICMP_TRACE_ID.fetch_add(1, Ordering::Relaxed);
    if id == 0 {
        // Counter wrapped to 0; consume one more slot to skip the wildcard value.
        id = NEXT_ICMP_TRACE_ID.fetch_add(1, Ordering::Relaxed);
    }
    id
}

/// Per-probe read timeout inside a round.
const READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Grace period after the target responds before the round is considered
/// complete (allows late destination replies to be collected).
///
/// 500ms covers late destination replies on >200ms RTT paths; the previous
/// 100ms dropped ~38% of destination replies on long-RTT paths under the
/// per-round-fresh-socket pattern.
const GRACE_DURATION: Duration = Duration::from_millis(500);

/// Sentinel value indicating a target has not published a TCP/UDP port.
///
/// We require a concrete port for TCP/UDP tracing; if the target doesn't
/// carry one the prober emits an error observation rather than probing a
/// bogus port.
const UNSET_PORT: u16 = 0;

/// Bounded round count per tracer build.
///
/// `Tracer::run_with` has no in-band cancel; its blocking thread can only
/// exit when `max_rounds` is reached or a network error occurs. Capping
/// at this value forces a natural exit + rebuild so shutdown and protocol
/// changes never leak the OS thread or raw socket. At 1 pps that's ≈ 1 hour
/// between rebuilds — long enough to preserve the persistent-socket benefit
/// (kernel-delivered late replies reach the same socket that sent them)
/// without unbounded thread leaks.
const ROUNDS_PER_TRACER: usize = 3600;

/// Compute the round-duration parameter from a positive `pps` value.
/// Both call sites in `run` (outer build + inner config-change handler)
/// use this so the comparison `new_min == min_round` is reliable.
fn min_round_from_pps(pps: f64) -> Duration {
    Duration::from_secs_f64((1.0 / pps.max(0.001)).max(0.001))
}

/// Shared trippy prober. One instance per agent; `spawn_target` attaches
/// a per-target task. The internal semaphore caps concurrent raw-socket
/// tracers across every target.
pub struct TrippyProber {
    semaphore: Arc<Semaphore>,
    cancel: CancellationToken,
}

impl TrippyProber {
    /// Build the prober with `concurrency` simultaneous rounds.
    pub fn new(concurrency: usize, cancel: CancellationToken) -> Arc<Self> {
        assert!(concurrency > 0, "trippy concurrency must be > 0");
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(concurrency)),
            cancel,
        })
    }

    /// Spawn a per-target trippy task.
    pub fn spawn_target(
        self: &Arc<Self>,
        target: Target,
        config_rx: watch::Receiver<TrippyRate>,
        route_trace_tx: mpsc::Sender<RouteTraceMsg>,
        allowlist_rx: watch::Receiver<Arc<HashSet<IpAddr>>>,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let ip = match meshmon_protocol::ip::to_ipaddr(&target.ip) {
            Ok(ip) => ip,
            Err(e) => {
                tracing::error!(target_id = %target.id, error = %e, "invalid target ip");
                return tokio::spawn(async {});
            }
        };
        let tcp_port = u16::try_from(target.tcp_probe_port)
            .ok()
            .filter(|&p| p != UNSET_PORT);
        let udp_port = u16::try_from(target.udp_probe_port)
            .ok()
            .filter(|&p| p != UNSET_PORT);

        let pool = Arc::clone(self);
        let target_id = target.id.clone();
        tokio::spawn(async move {
            run(
                pool,
                target_id,
                ip,
                tcp_port,
                udp_port,
                config_rx,
                route_trace_tx,
                allowlist_rx,
                cancel,
            )
            .await;
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    pool: Arc<TrippyProber>,
    target_id: String,
    target_ip: IpAddr,
    tcp_port: Option<u16>,
    udp_port: Option<u16>,
    mut config_rx: watch::Receiver<TrippyRate>,
    route_trace_tx: mpsc::Sender<RouteTraceMsg>,
    allowlist_rx: watch::Receiver<Arc<HashSet<IpAddr>>>,
    cancel: CancellationToken,
) {
    // Track current build params so we know when to rebuild.
    let mut current: Option<(Protocol, Duration)> = None;

    loop {
        let rate = *config_rx.borrow();

        // Idle: no usable rate yet.
        if rate.protocol == Protocol::Unspecified || !rate.pps.is_finite() || rate.pps <= 0.0 {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = pool.cancel.cancelled() => return,
                res = config_rx.changed() => {
                    if res.is_err() { return; }
                    continue;
                }
            }
        }

        let min_round = min_round_from_pps(rate.pps);

        // Rebuild only on protocol change. PPS adjustments do NOT force a rebuild;
        // the persistent tracer keeps its build-time cadence until either the
        // protocol swings or the bounded `ROUNDS_PER_TRACER` count expires
        // (forcing a natural rebuild via the closed bridge channel).
        let needs_rebuild = current.as_ref().is_none_or(|(p, _)| *p != rate.protocol);

        if !needs_rebuild {
            // Same (protocol, pps) — wait for a change or cancel.
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = pool.cancel.cancelled() => return,
                res = config_rx.changed() => {
                    if res.is_err() { return; }
                    continue;
                }
            }
        }

        // Acquire semaphore permit for the lifetime of this persistent tracer.
        let permit = match pool.semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => return, // semaphore closed
        };

        // Build a fresh tracer for this (protocol, pps) pair.
        let cfg = match build_config_for(target_ip, rate.protocol, tcp_port, udp_port, min_round) {
            Ok(c) => c,
            Err(e) => {
                drop(permit);
                tracing::error!(target_id = %target_id, error = %e, "trippy build_config_for failed");
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = pool.cancel.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => continue,
                }
            }
        };

        let tracer = match cfg.builder.build() {
            Ok(t) => Arc::new(t),
            Err(e) => {
                drop(permit);
                tracing::error!(target_id = %target_id, error = %e, "trippy tracer build failed");
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = pool.cancel.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => continue,
                }
            }
        };

        current = Some((rate.protocol, min_round));

        // Bridge channel: callback (sync) → async drain.
        // Capacity 8 absorbs burst; if the async side falls behind we drop rounds.
        let (round_tx, mut round_rx) = mpsc::channel::<Vec<HopObservation>>(8);

        // Spawn the blocking tracer worker.
        let tracer_for_blocking = tracer.clone();
        let target_id_for_blocking = target_id.clone();
        let blocking = tokio::task::spawn_blocking(move || {
            // run_with returns when ROUNDS_PER_TRACER is reached or a fatal
            // error occurs. The callback fires after each completed round.
            let result = tracer_for_blocking.run_with(|_round| {
                // The snapshot reflects per-round-fresh state because the
                // callback calls `clear()` after sending; trippy-core's
                // strategy releases the State write lock before invoking the
                // callback, so snapshot() acquires only the read lock and
                // never deadlocks.
                let state: State = tracer_for_blocking.snapshot();
                let hops: Vec<HopObservation> = state
                    .hops()
                    .iter()
                    .map(|hop| HopObservation {
                        position: hop.ttl(),
                        ip: hop.addrs().next().copied(),
                        rtt_micros: hop.best_ms().map(ms_to_micros),
                    })
                    .collect();
                let _ = round_tx.blocking_send(hops);
                // Reset cumulative state so the NEXT round's snapshot reflects
                // only that round's probes. Without this the cumulative `best_ms`
                // would dominate downstream rolling-window math (the tracker
                // would see the all-time best RTT replayed every round).
                tracer_for_blocking.clear();
            });
            if let Err(e) = result {
                tracing::error!(
                    target_id = %target_id_for_blocking,
                    error = %e,
                    "trippy persistent tracer exited with error",
                );
            } else {
                tracing::debug!(
                    target_id = %target_id_for_blocking,
                    "trippy persistent tracer exited cleanly (round limit reached or shutdown)",
                );
            }
            // Permit was held for the full tracer lifetime; drop on exit.
            drop(permit);
        });

        // Inner async loop: drain rounds + watch for cancel/config change.
        let mut needs_rebuild_signal = false;
        'inner: loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    break 'inner;
                }
                _ = pool.cancel.cancelled() => {
                    break 'inner;
                }
                res = config_rx.changed() => {
                    if res.is_err() {
                        needs_rebuild_signal = true;
                        break 'inner;
                    }
                    let new_rate = *config_rx.borrow();
                    if new_rate.protocol != rate.protocol {
                        // Protocol swing: rebuild. PPS changes alone do not
                        // rebuild (the persistent tracer keeps its build-time
                        // cadence until ROUNDS_PER_TRACER expires).
                        needs_rebuild_signal = true;
                        break 'inner;
                    }
                }
                maybe_hops = round_rx.recv() => {
                    let Some(hops) = maybe_hops else {
                        // Bridge channel closed — blocking task exited unexpectedly.
                        needs_rebuild_signal = true;
                        break 'inner;
                    };
                    if hops.is_empty() {
                        continue;
                    }
                    let hit = {
                        let allowlist = allowlist_rx.borrow();
                        detect_contamination(target_ip, &hops, allowlist.as_ref())
                    };
                    if let Some(hit) = hit {
                        CROSS_CONTAMINATION_TOTAL.fetch_add(1, Ordering::Relaxed);
                        warn_contamination_if_due(&target_id, &hit);
                        continue;
                    }
                    let trace = RouteTraceMsg {
                        target_id: target_id.clone(),
                        protocol: rate.protocol,
                        hops,
                        observed_at: tokio::time::Instant::now(),
                    };
                    if route_trace_tx.send(trace).await.is_err() {
                        // Supervisor gone — exit entirely.
                        drop(round_rx);
                        drop(tracer);
                        let _ = tokio::time::timeout(Duration::from_secs(15), blocking).await;
                        return;
                    }
                }
            }
        }

        // Trigger shutdown: drop bridge receiver so the next blocking_send fails,
        // which causes run_with to keep cycling but every send errors. Then drop
        // the tracer Arc to close the raw socket when run_with returns.
        drop(round_rx);
        drop(tracer);
        // Give the blocking task up to 15s to drain and exit.
        let _ = tokio::time::timeout(Duration::from_secs(15), blocking).await;

        if !needs_rebuild_signal {
            // cancel fired or pool cancel fired — exit.
            return;
        }
        // Loop back and rebuild with the latest config.
    }
}

/// Per-protocol `trippy_core::Builder` configuration summary. `pub(super)`
/// so unit tests can assert what the prober hands to trippy without
/// running a raw-socket probe.
///
/// Used by the persistent-tracer loop in `run` (which calls `Tracer::run_with`
/// for `ROUNDS_PER_TRACER` rounds and then rebuilds) and by the loopback
/// unit tests (which override `max_rounds` to 1 for one-shot probes).
#[cfg_attr(test, derive(Debug))]
pub(super) struct TrippyBuildConfig {
    /// `None` for TCP/UDP (trippy-core matches replies on ports/address);
    /// `Some(id)` for ICMP (matched against the echoed identifier).
    ///
    /// Read by unit tests; the non-test build writes but does not read it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub trace_identifier: Option<u16>,
    pub builder: Builder,
}

/// Build per-protocol trippy builder configuration.
///
/// `min_round_duration` pins the round cadence: both `min_round_duration`
/// and `max_round_duration` are set to this value so trippy does not add
/// internal jitter. PPS changes require a rebuild (no live rate adjustment
/// is available in trippy-core 0.13).
pub(super) fn build_config_for(
    target_ip: IpAddr,
    protocol: Protocol,
    tcp_port: Option<u16>,
    udp_port: Option<u16>,
    min_round_duration: Duration,
) -> Result<TrippyBuildConfig, anyhow::Error> {
    let trippy_proto = match protocol {
        Protocol::Icmp => trippy_core::Protocol::Icmp,
        Protocol::Tcp => trippy_core::Protocol::Tcp,
        Protocol::Udp => trippy_core::Protocol::Udp,
        Protocol::Unspecified => anyhow::bail!("trippy: UNSPECIFIED protocol"),
    };

    let mut builder = Builder::new(target_ip)
        .protocol(trippy_proto)
        .max_ttl(MAX_TTL)
        .read_timeout(READ_TIMEOUT)
        .grace_duration(GRACE_DURATION)
        .min_round_duration(min_round_duration)
        .max_round_duration(min_round_duration) // pin: no internal jitter at the trippy layer
        .max_rounds(Some(ROUNDS_PER_TRACER));

    let mut trace_identifier: Option<u16> = None;
    if matches!(protocol, Protocol::Icmp) {
        let id = next_trace_id();
        builder = builder.trace_identifier(id);
        trace_identifier = Some(id);
    }

    // TCP/UDP tracing requires a concrete destination port (via
    // PortDirection::FixedDest — trippy-core 0.13 does not accept
    // PortDirection::None for TCP/UDP). ICMP tracing leaves the port
    // direction as the default.
    builder = match protocol {
        Protocol::Tcp => {
            let port = tcp_port
                .ok_or_else(|| anyhow::anyhow!("trippy: tcp protocol without tcp_probe_port"))?;
            builder.port_direction(PortDirection::FixedDest(Port(port)))
        }
        Protocol::Udp => {
            let port = udp_port
                .ok_or_else(|| anyhow::anyhow!("trippy: udp protocol without udp_probe_port"))?;
            builder.port_direction(PortDirection::FixedDest(Port(port)))
        }
        // `Unspecified` already bailed above; `Icmp` needs no port direction.
        Protocol::Icmp | Protocol::Unspecified => builder,
    };

    Ok(TrippyBuildConfig {
        trace_identifier,
        builder,
    })
}

/// Which foreign peer IP showed up at which hop position in a round that
/// wasn't meant for us. Produced by `detect_contamination`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContaminationHit {
    pub position: u8,
    pub foreign_ip: IpAddr,
}

/// Walks `hops` in order and returns the first hop whose IP is in
/// `allowlist` but not equal to `target_ip`. `None` means the round is
/// clean. The destination IP appearing at its own position (or beyond,
/// via trippy over-probing) is never flagged — only sibling targets are.
///
/// Both `target_ip` and each hop IP are canonicalized before comparison.
/// `target_ip` is already canonical in most call paths (bootstrap +
/// `RouteTracker::new` both normalize), but `TrippyProber::spawn_target`
/// / `run` pass the raw decoded IP straight through, so we canonicalize
/// here as belt-and-suspenders. Without this, an IPv4-mapped-IPv6
/// target would never match its own canonical destination hop, and
/// every legitimate destination round would be silently discarded as
/// contamination. The allowlist is populated from canonical IPs (see
/// `publish_allowlist` in bootstrap.rs) so hop-side canonicalization
/// matters equally.
pub(super) fn detect_contamination(
    target_ip: IpAddr,
    hops: &[HopObservation],
    allowlist: &HashSet<IpAddr>,
) -> Option<ContaminationHit> {
    let target_ip = target_ip.to_canonical();
    for hop in hops {
        if let Some(ip) = hop.ip {
            let ip = ip.to_canonical();
            if ip != target_ip && allowlist.contains(&ip) {
                return Some(ContaminationHit {
                    position: hop.position,
                    foreign_ip: ip,
                });
            }
        }
    }
    None
}

static CROSS_CONTAMINATION_TOTAL: AtomicU64 = AtomicU64::new(0);

pub(crate) fn cross_contamination_total() -> u64 {
    CROSS_CONTAMINATION_TOTAL.load(Ordering::Relaxed)
}

/// Monotonic nanos timestamp of the last contamination warn emission.
/// `u64::MAX` is the "never fired" sentinel; any other value is a real
/// nanos-since-epoch stamp.
///
/// `u64::MAX` is safe as a sentinel because `now_nanos_since_epoch()`
/// caps its output at `u64::MAX`, and ~584 years of continuous runtime
/// would be required to reach that value naturally — at which point the
/// warn would have fired countless times and the slot would carry a
/// smaller stamp.
///
/// Rationale for not using `0`: under `#[tokio::test(start_paused = true)]`
/// a sustained burst can observe `now_nanos == 0` for every call before
/// the first `tokio::time::advance`, which would trip a `prev != 0` gate
/// every time and defeat the throttle.
static LAST_CONTAMINATION_WARN_NANOS: AtomicU64 = AtomicU64::new(u64::MAX);

/// Minimum interval between consecutive contamination warn emissions.
const CONTAMINATION_WARN_COOLDOWN: Duration = Duration::from_secs(60);

/// Epoch captured once at first use. Measuring `duration_since` this epoch
/// gives a monotonically increasing nanos value suitable for comparison
/// against `LAST_CONTAMINATION_WARN_NANOS`.
///
/// Under `#[tokio::test(start_paused = true)]` this uses the paused clock,
/// so `tokio::time::advance()` moves the epoch-relative value forward and
/// the cooldown gate responds correctly in tests.
static CONTAMINATION_WARN_EPOCH: LazyLock<tokio::time::Instant> =
    LazyLock::new(tokio::time::Instant::now);

/// Test-only counter incremented each time the cooldown gate opens and a
/// warn is about to be emitted. Allows tests to assert exact warn counts
/// without scraping log output.
#[cfg(test)]
static WARN_EMITTED_FOR_TEST: AtomicU64 = AtomicU64::new(0);

fn now_nanos_since_epoch() -> u64 {
    tokio::time::Instant::now()
        .duration_since(*CONTAMINATION_WARN_EPOCH)
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

/// Returns `true` at most once every [`CONTAMINATION_WARN_COOLDOWN`].
///
/// Uses a compare-exchange on a monotonic nanos counter. The first call after
/// process start (or `reset_contamination_state_for_test`) always returns
/// `true` because `LAST_CONTAMINATION_WARN_NANOS` starts at `u64::MAX`
/// (the "never fired" sentinel). Callers that lose the CAS race suppress
/// their own warn while counter fidelity is unaffected.
///
/// Scope is **process-wide**, not per-target: concurrent contamination
/// across multiple targets within the same 60 s window produces at most
/// one warn. This is a deliberate deviation from the plan's §3 wording
/// — a single operator doesn't need `60 s × N-targets` worth of warn
/// volume, and [`cross_contamination_total`] still reflects every hit for
/// metrics/alerting purposes.
fn warn_cooldown_elapsed() -> bool {
    let now_nanos = now_nanos_since_epoch();
    loop {
        let prev = LAST_CONTAMINATION_WARN_NANOS.load(Ordering::Relaxed);
        // prev == u64::MAX → never fired; gate is open.
        // prev != u64::MAX and elapsed < cooldown → gate is closed.
        if prev != u64::MAX
            && now_nanos.saturating_sub(prev) < CONTAMINATION_WARN_COOLDOWN.as_nanos() as u64
        {
            return false;
        }
        // Atomically capture the slot. If another caller beat us, retry —
        // their write is also `now_nanos`-ish, so the next loop iteration
        // will fail the elapsed check and return false.
        if LAST_CONTAMINATION_WARN_NANOS
            .compare_exchange(prev, now_nanos, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
    }
}

fn warn_contamination_if_due(target_id: &str, hit: &ContaminationHit) {
    if !warn_cooldown_elapsed() {
        return;
    }
    #[cfg(test)]
    WARN_EMITTED_FOR_TEST.fetch_add(1, Ordering::Relaxed);
    tracing::warn!(
        target_id = %target_id,
        foreign_ip = %hit.foreign_ip,
        position = hit.position,
        contamination_total = cross_contamination_total(),
        cooldown_sec = CONTAMINATION_WARN_COOLDOWN.as_secs(),
        "trippy round cross-contamination detected; discarding observation",
    );
}

/// Run one trippy round synchronously. Callers must wrap this in
/// `spawn_blocking` — trippy-core 0.13 performs raw-socket I/O on the
/// calling thread.
///
/// Returns a `ProbeObservation` so the existing `Builder::build()`-driven
/// loopback tests below can keep their shape. `max_rounds(Some(1))` is set
/// internally so this helper always stops after a single round.
#[cfg(test)]
fn run_one_round(
    target_id: &str,
    target_ip: IpAddr,
    protocol: Protocol,
    tcp_port: Option<u16>,
    udp_port: Option<u16>,
) -> Result<crate::probing::ProbeObservation, anyhow::Error> {
    let cfg = build_config_for(
        target_ip,
        protocol,
        tcp_port,
        udp_port,
        Duration::from_secs(1),
    )?;
    // One-shot: max_rounds(Some(1)) so run() returns after a single round.
    let tracer = cfg
        .builder
        .max_rounds(Some(1))
        .build()
        .map_err(|e| anyhow::anyhow!("trippy build: {e}"))?;
    tracer
        .run()
        .map_err(|e| anyhow::anyhow!("trippy run: {e}"))?;

    let state: State = tracer.snapshot();
    let hops = state
        .hops()
        .iter()
        .map(|hop| HopObservation {
            position: hop.ttl(),
            ip: hop.addrs().next().copied(),
            rtt_micros: hop.best_ms().map(ms_to_micros),
        })
        .collect::<Vec<_>>();

    // `State::target_hop` returns a hop keyed by the current round's
    // highest TTL — even for the default sentinel it never panics.
    let target_hop = state.target_hop(State::default_flow_id());
    let outcome = if target_hop.total_recv() == 0 || target_hop.addrs().next().is_none() {
        crate::probing::ProbeOutcome::Timeout
    } else {
        match target_hop.best_ms() {
            Some(ms) => crate::probing::ProbeOutcome::Success {
                rtt_micros: ms_to_micros(ms),
            },
            None => crate::probing::ProbeOutcome::Timeout,
        }
    };

    Ok(crate::probing::ProbeObservation {
        protocol,
        target_id: target_id.to_string(),
        outcome,
        hops: Some(hops),
        observed_at: tokio::time::Instant::now(),
    })
}

/// Convert milliseconds to microseconds as `u32`. Returns `0` for
/// non-finite or non-positive inputs; saturates at [`u32::MAX`] on
/// overflow.
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

#[cfg(test)]
pub(super) fn reset_contamination_state_for_test() {
    CROSS_CONTAMINATION_TOTAL.store(0, Ordering::Relaxed);
    LAST_CONTAMINATION_WARN_NANOS.store(u64::MAX, Ordering::Relaxed);
    WARN_EMITTED_FOR_TEST.store(0, Ordering::Relaxed);
    // Force LazyLock initialization so that subsequent `tokio::time::advance`
    // calls have a fixed epoch to measure against (the paused clock's origin).
    let _ = *CONTAMINATION_WARN_EPOCH;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn min_round_from_pps_clamps_to_one_ms_floor() {
        // Very high pps must not collapse to a sub-millisecond round duration.
        assert!(min_round_from_pps(10_000.0) >= Duration::from_millis(1));
        // Zero or negative pps must also clamp (defensive — caller should
        // already filter these via the idle path, but the helper is total).
        assert!(min_round_from_pps(0.0) >= Duration::from_millis(1));
        assert!(min_round_from_pps(-1.0) >= Duration::from_millis(1));
        // Reasonable pps maps as expected.
        assert_eq!(min_round_from_pps(1.0), Duration::from_secs(1));
    }

    #[test]
    fn ms_to_micros_handles_edges() {
        assert_eq!(ms_to_micros(0.0), 0);
        assert_eq!(ms_to_micros(-1.0), 0);
        assert_eq!(ms_to_micros(f64::NAN), 0);
        assert_eq!(ms_to_micros(f64::INFINITY), 0);
        assert_eq!(ms_to_micros(1.5), 1500);
        assert_eq!(ms_to_micros(1.0e12), u32::MAX);
    }

    /// Smoke test that `run_one_round` can build + run a tracer on
    /// loopback. Requires `CAP_NET_RAW` (or root), so ignored by default.
    #[tokio::test]
    #[ignore = "requires CAP_NET_RAW"]
    async fn trippy_loopback_icmp_round() {
        let obs = tokio::task::spawn_blocking(|| {
            run_one_round(
                "self",
                "127.0.0.1".parse().unwrap(),
                Protocol::Icmp,
                None,
                None,
            )
        })
        .await
        .expect("blocking task panicked")
        .expect("trippy round should succeed on loopback with caps");
        assert_eq!(obs.target_id, "self");
        assert!(
            obs.hops.as_ref().map(|h| !h.is_empty()).unwrap_or(false),
            "expected at least one hop: {obs:?}"
        );
    }

    #[test]
    #[serial]
    fn next_trace_id_is_nonzero() {
        for _ in 0..100 {
            assert_ne!(next_trace_id(), 0);
        }
    }

    #[test]
    #[serial]
    fn next_trace_id_is_monotonically_distinct_for_1000_calls() {
        use std::collections::HashSet;
        let mut seen: HashSet<u16> = HashSet::new();
        for _ in 0..1000 {
            let id = next_trace_id();
            assert!(
                seen.insert(id),
                "duplicate trace id {id} within 1000 calls (counter not advanced?)",
            );
        }
    }

    // NOTE: `NEXT_ICMP_TRACE_ID` is process-wide. This test mutates it
    // directly, so it must serialize with any other test that calls
    // `next_trace_id()`. Enforced via the `#[serial]` annotations above.
    #[test]
    #[serial]
    fn next_trace_id_skips_zero_after_wrap() {
        // Force LazyLock initialization by calling next_trace_id once.
        let _ = next_trace_id();
        // Seed the counter so the next fetch_add returns u16::MAX,
        // the one after wraps to 0, which next_trace_id must skip.
        NEXT_ICMP_TRACE_ID.store(u16::MAX, std::sync::atomic::Ordering::Relaxed);
        let a = next_trace_id(); // returns u16::MAX, increments to 0
        let b = next_trace_id(); // would return 0, must consume one more → 1
        assert_eq!(a, u16::MAX);
        assert_eq!(b, 1, "post-wrap id must skip 0");
    }

    #[test]
    #[serial]
    fn icmp_build_config_uses_nonzero_trace_identifier() {
        let ip: IpAddr = "1.1.1.1".parse().unwrap();
        let cfg = build_config_for(ip, Protocol::Icmp, None, None, Duration::from_secs(1))
            .expect("config");
        assert!(cfg.trace_identifier.is_some(), "ICMP must set a trace id");
        assert_ne!(cfg.trace_identifier, Some(0), "must be non-zero");
    }

    #[test]
    fn tcp_build_config_does_not_set_trace_identifier() {
        let ip: IpAddr = "1.1.1.1".parse().unwrap();
        let cfg = build_config_for(ip, Protocol::Tcp, Some(443), None, Duration::from_secs(1))
            .expect("config");
        assert_eq!(cfg.trace_identifier, None, "TCP does not set trace id");
    }

    #[test]
    fn udp_build_config_does_not_set_trace_identifier() {
        let ip: IpAddr = "1.1.1.1".parse().unwrap();
        let cfg = build_config_for(ip, Protocol::Udp, None, Some(33434), Duration::from_secs(1))
            .expect("config");
        assert_eq!(cfg.trace_identifier, None, "UDP does not set trace id");
    }

    #[tokio::test]
    async fn trippy_prober_spawn_target_accepts_allowlist_rx() {
        // Compile-level assertion: the new 5-arg spawn_target signature
        // is accepted. No runtime behavior is exercised (spawn_target
        // immediately sees a cancelled token and returns).
        use std::collections::HashSet;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let cancel = CancellationToken::new();
        let prober = TrippyProber::new(1, cancel.clone());
        let (_config_tx, config_rx) = tokio::sync::watch::channel(TrippyRate::idle());
        let (route_trace_tx, _route_trace_rx) =
            tokio::sync::mpsc::channel::<crate::probing::RouteTraceMsg>(8);
        let (_allow_tx, allowlist_rx) =
            tokio::sync::watch::channel::<Arc<HashSet<std::net::IpAddr>>>(Arc::new(HashSet::new()));

        let target = meshmon_protocol::Target {
            id: "peer".into(),
            ip: vec![127, 0, 0, 1].into(),
            display_name: "Peer".into(),
            location: "Test".into(),
            lat: 0.0,
            lon: 0.0,
            tcp_probe_port: 0,
            udp_probe_port: 0,
        };

        let handle = prober.spawn_target(
            target,
            config_rx,
            route_trace_tx,
            allowlist_rx,
            cancel.clone(),
        );
        cancel.cancel();
        let _ = handle.await;
    }

    #[test]
    fn clean_round_is_not_contamination() {
        let me: IpAddr = "10.0.0.1".parse().unwrap();
        let allowlist = ["10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()]
            .into_iter()
            .collect::<HashSet<IpAddr>>();
        let hops = vec![
            HopObservation {
                position: 1,
                ip: Some("192.168.0.1".parse().unwrap()),
                rtt_micros: Some(100),
            },
            HopObservation {
                position: 2,
                ip: Some("10.0.0.1".parse().unwrap()),
                rtt_micros: Some(200),
            },
        ];
        assert_eq!(detect_contamination(me, &hops, &allowlist), None);
    }

    #[test]
    fn foreign_target_ip_at_any_hop_is_contamination() {
        let me: IpAddr = "10.0.0.1".parse().unwrap();
        let allowlist = ["10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()]
            .into_iter()
            .collect::<HashSet<IpAddr>>();
        let hops = vec![
            HopObservation {
                position: 1,
                ip: Some("192.168.0.1".parse().unwrap()),
                rtt_micros: Some(100),
            },
            HopObservation {
                position: 2,
                ip: Some("10.0.0.2".parse().unwrap()),
                rtt_micros: Some(200),
            },
        ];
        let hit = detect_contamination(me, &hops, &allowlist).expect("contaminated");
        assert_eq!(hit.position, 2);
        assert_eq!(hit.foreign_ip, "10.0.0.2".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn own_target_ip_at_any_hop_is_not_contamination() {
        let me: IpAddr = "10.0.0.1".parse().unwrap();
        let allowlist = ["10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()]
            .into_iter()
            .collect::<HashSet<IpAddr>>();
        let hops = vec![
            HopObservation {
                position: 13,
                ip: Some(me),
                rtt_micros: Some(1000),
            },
            HopObservation {
                position: 14,
                ip: Some(me),
                rtt_micros: Some(1000),
            },
        ];
        assert_eq!(detect_contamination(me, &hops, &allowlist), None);
    }

    #[test]
    fn ip_outside_allowlist_is_not_contamination() {
        let me: IpAddr = "10.0.0.1".parse().unwrap();
        let allowlist = ["10.0.0.1".parse().unwrap()]
            .into_iter()
            .collect::<HashSet<IpAddr>>();
        let hops = vec![HopObservation {
            position: 5,
            ip: Some("8.8.8.8".parse().unwrap()),
            rtt_micros: Some(15000),
        }];
        assert_eq!(detect_contamination(me, &hops, &allowlist), None);
    }

    #[test]
    fn multiple_contaminations_report_first_in_order() {
        let me: IpAddr = "10.0.0.1".parse().unwrap();
        let a: IpAddr = "10.0.0.2".parse().unwrap();
        let b: IpAddr = "10.0.0.3".parse().unwrap();
        let allowlist: HashSet<IpAddr> = [me, a, b].into_iter().collect();
        let hops = vec![
            HopObservation {
                position: 2,
                ip: Some(b),
                rtt_micros: Some(100),
            },
            HopObservation {
                position: 5,
                ip: Some(a),
                rtt_micros: Some(300),
            },
        ];
        let hit = detect_contamination(me, &hops, &allowlist).expect("hit");
        assert_eq!(hit.foreign_ip, b);
        assert_eq!(hit.position, 2);
    }

    #[test]
    fn contamination_handles_ipv4_mapped_ipv6_target() {
        let me: IpAddr = "10.0.0.1".parse().unwrap();
        let allowlist: HashSet<IpAddr> = ["10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()]
            .into_iter()
            .collect();
        // Hop reports the target as IPv4-mapped-IPv6; must NOT be flagged as contamination.
        let mapped: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        let hops = vec![HopObservation {
            position: 1,
            ip: Some(mapped),
            rtt_micros: Some(100),
        }];
        assert_eq!(detect_contamination(me, &hops, &allowlist), None);
    }

    #[test]
    fn contamination_flags_ipv4_mapped_foreign_peer() {
        let me: IpAddr = "10.0.0.1".parse().unwrap();
        let allowlist: HashSet<IpAddr> = ["10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()]
            .into_iter()
            .collect();
        // Foreign sibling reported as IPv4-mapped-IPv6 — must still flag.
        let mapped_foreign: IpAddr = "::ffff:10.0.0.2".parse().unwrap();
        let hops = vec![HopObservation {
            position: 5,
            ip: Some(mapped_foreign),
            rtt_micros: Some(200),
        }];
        let hit = detect_contamination(me, &hops, &allowlist).expect("should flag");
        assert_eq!(
            hit.foreign_ip,
            "10.0.0.2".parse::<IpAddr>().unwrap(),
            "foreign_ip reported as canonical"
        );
        assert_eq!(hit.position, 5);
    }

    #[test]
    fn contamination_handles_canonical_destination_when_target_is_mapped_v6() {
        // Inverse of contamination_handles_ipv4_mapped_ipv6_target: the caller
        // supplies target_ip in IPv4-mapped-IPv6 form (as TrippyProber::run
        // does when trippy-core hands back a mapped-v6 destination) while the
        // hop reports the destination in canonical IPv4 form. Without the
        // target-side canonicalization inside detect_contamination, this
        // would silently drop every legitimate destination round.
        let me: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        let allowlist: HashSet<IpAddr> = ["10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()]
            .into_iter()
            .collect();
        let hops = vec![HopObservation {
            position: 13,
            ip: Some("10.0.0.1".parse().unwrap()),
            rtt_micros: Some(260_000),
        }];
        assert_eq!(
            detect_contamination(me, &hops, &allowlist),
            None,
            "canonical hop must match mapped-v6 target",
        );
    }

    // --- warn rate-limiting tests ---
    //
    // All three tests use `#[tokio::test(start_paused = true)]` so the
    // process-wide `CONTAMINATION_WARN_EPOCH: LazyLock<Instant>` is captured
    // against the paused tokio clock regardless of execution order. Mixing
    // a plain `#[test]` would risk pinning the epoch to the real clock if
    // it ran first, which would break the paused-clock tests that follow.

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn warn_emitted_at_first_contamination() {
        reset_contamination_state_for_test();
        let hit = ContaminationHit {
            position: 3,
            foreign_ip: "10.0.0.2".parse().unwrap(),
        };
        CROSS_CONTAMINATION_TOTAL.fetch_add(1, Ordering::Relaxed);
        warn_contamination_if_due("peer-id", &hit);
        assert_eq!(
            WARN_EMITTED_FOR_TEST.load(Ordering::Relaxed),
            1,
            "first contamination must emit a warn"
        );
        assert_eq!(cross_contamination_total(), 1);
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn warn_throttled_under_sustained_contamination() {
        reset_contamination_state_for_test();
        let hit = ContaminationHit {
            position: 3,
            foreign_ip: "10.0.0.2".parse().unwrap(),
        };

        for _ in 0..10 {
            CROSS_CONTAMINATION_TOTAL.fetch_add(1, Ordering::Relaxed);
            warn_contamination_if_due("peer-id", &hit);
            // No time advance — all calls are within the cooldown window.
        }

        assert_eq!(
            WARN_EMITTED_FOR_TEST.load(Ordering::Relaxed),
            1,
            "expected exactly 1 warn within cooldown window across 10 hits"
        );
        assert_eq!(
            cross_contamination_total(),
            10,
            "counter must reflect every hit regardless of warn throttle"
        );
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn warn_re_emits_after_cooldown() {
        reset_contamination_state_for_test();
        let hit = ContaminationHit {
            position: 3,
            foreign_ip: "10.0.0.2".parse().unwrap(),
        };

        CROSS_CONTAMINATION_TOTAL.fetch_add(1, Ordering::Relaxed);
        warn_contamination_if_due("peer-id", &hit);
        assert_eq!(WARN_EMITTED_FOR_TEST.load(Ordering::Relaxed), 1);

        // Advance past the 60s cooldown.
        tokio::time::advance(std::time::Duration::from_secs(61)).await;

        CROSS_CONTAMINATION_TOTAL.fetch_add(1, Ordering::Relaxed);
        warn_contamination_if_due("peer-id", &hit);
        assert_eq!(
            WARN_EMITTED_FOR_TEST.load(Ordering::Relaxed),
            2,
            "expected 2 warns: one before and one after the cooldown expires"
        );
    }

    // --- integration test — contaminated rounds never reach downstream ---

    /// Replays pre-built [`RouteTraceMsg`]s through the contamination-detection
    /// path used by `run()`'s inner loop: borrow the allowlist, call
    /// `detect_contamination`, bump the counter and drop on a hit, forward to
    /// `route_trace_tx` on a clean round.
    ///
    /// This is intentionally **not** the real supervisor — it has no raw sockets,
    /// no `spawn_blocking`, and no timing logic. Its only purpose is to exercise
    /// the contamination-detection branch in a deterministic, no-privilege way.
    async fn spawn_scripted_trippy_for_test(
        target_ip: IpAddr,
        allowlist_rx: watch::Receiver<Arc<HashSet<IpAddr>>>,
        route_trace_tx: mpsc::Sender<crate::probing::RouteTraceMsg>,
        rounds: Vec<crate::probing::RouteTraceMsg>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            for msg in rounds {
                let hit = {
                    let allowlist = allowlist_rx.borrow();
                    detect_contamination(target_ip, &msg.hops, allowlist.as_ref())
                };
                if hit.is_some() {
                    CROSS_CONTAMINATION_TOTAL.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if route_trace_tx.send(msg).await.is_err() {
                    return;
                }
            }
        })
    }

    #[tokio::test]
    #[serial]
    async fn supervisor_discards_contaminated_round_does_not_feed_stats_or_tracker() {
        // Reset counter so the delta is observable deterministically.
        reset_contamination_state_for_test();

        let target_ip: IpAddr = "45.248.78.119".parse().unwrap();
        let foreign_ip: IpAddr = "146.185.214.131".parse().unwrap();

        let (_allow_tx, allowlist_rx) = tokio::sync::watch::channel(Arc::new(
            [target_ip, foreign_ip]
                .into_iter()
                .collect::<HashSet<IpAddr>>(),
        ));

        let (route_tx, mut route_rx) =
            tokio::sync::mpsc::channel::<crate::probing::RouteTraceMsg>(8);

        let clean_round_1 = crate::probing::RouteTraceMsg {
            protocol: Protocol::Icmp,
            target_id: "au-west".into(),
            hops: vec![HopObservation {
                position: 1,
                ip: Some(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1))),
                rtt_micros: Some(100),
            }],
            observed_at: tokio::time::Instant::now(),
        };
        let contaminated_round = crate::probing::RouteTraceMsg {
            protocol: Protocol::Icmp,
            target_id: "au-west".into(),
            hops: vec![HopObservation {
                position: 5,
                ip: Some(foreign_ip),
                rtt_micros: Some(200),
            }],
            observed_at: tokio::time::Instant::now(),
        };
        let clean_round_2 = crate::probing::RouteTraceMsg {
            protocol: Protocol::Icmp,
            target_id: "au-west".into(),
            hops: vec![HopObservation {
                position: 1,
                ip: Some(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2))),
                rtt_micros: Some(150),
            }],
            observed_at: tokio::time::Instant::now(),
        };

        let handle = spawn_scripted_trippy_for_test(
            target_ip,
            allowlist_rx,
            route_tx.clone(),
            vec![
                clean_round_1.clone(),
                contaminated_round,
                clean_round_2.clone(),
            ],
        )
        .await;

        handle.await.expect("scripted task joins cleanly");
        drop(route_tx); // close the channel so recv drains

        let mut emitted: Vec<crate::probing::RouteTraceMsg> = Vec::new();
        while let Some(msg) = route_rx.recv().await {
            emitted.push(msg);
        }

        assert_eq!(emitted.len(), 2, "only 2 clean rounds reach downstream");
        assert_eq!(
            cross_contamination_total(),
            1,
            "one contaminated round counted"
        );

        // Preserve ordering: the contaminated round was dropped, so emitted[0]
        // corresponds to clean_round_1 and emitted[1] to clean_round_2.
        assert_eq!(
            emitted[0].hops[0].ip, clean_round_1.hops[0].ip,
            "first emitted round must be clean_round_1"
        );
        assert_eq!(
            emitted[1].hops[0].ip, clean_round_2.hops[0].ip,
            "second emitted round must be clean_round_2"
        );
    }

    #[tokio::test]
    async fn scripted_trippy_emits_route_trace_msg() {
        use crate::probing::{HopObservation, RouteTraceMsg};
        use std::collections::HashSet;
        use std::sync::Arc;

        let target_ip: IpAddr = "203.0.113.10".parse().unwrap();
        let (_allow_tx, allowlist_rx) = tokio::sync::watch::channel(Arc::new(HashSet::new()));
        let (route_tx, mut route_rx) = tokio::sync::mpsc::channel::<RouteTraceMsg>(8);

        let one_round = RouteTraceMsg {
            target_id: "au-west".into(),
            protocol: Protocol::Icmp,
            hops: vec![HopObservation {
                position: 1,
                ip: Some(target_ip),
                rtt_micros: Some(1_000),
            }],
            observed_at: tokio::time::Instant::now(),
        };

        let handle = spawn_scripted_trippy_for_test(
            target_ip,
            allowlist_rx,
            route_tx.clone(),
            vec![one_round.clone()],
        )
        .await;

        handle.await.expect("scripted task joins cleanly");
        drop(route_tx);

        let mut received: Vec<RouteTraceMsg> = Vec::new();
        while let Some(msg) = route_rx.recv().await {
            received.push(msg);
        }

        assert_eq!(received.len(), 1);
        assert_eq!(received[0].target_id, "au-west");
        assert_eq!(received[0].protocol, Protocol::Icmp);
        assert_eq!(received[0].hops.len(), 1);
        assert_eq!(received[0].hops[0].ip, Some(target_ip));
    }

    /// Verifies the persistent tracer loop emits multiple RouteTraceMsgs on
    /// loopback. Requires `CAP_NET_RAW` (or root), so ignored by default.
    /// Run with: `cargo test -p meshmon-agent -- --ignored persistent_tracer_emits_multiple_rounds_on_loopback`
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires CAP_NET_RAW; run with `cargo test -- --ignored`"]
    async fn persistent_tracer_emits_multiple_rounds_on_loopback() {
        use std::collections::HashSet;
        use std::sync::Arc;

        let cancel = CancellationToken::new();
        let prober = TrippyProber::new(4, cancel.clone());

        let (route_tx, mut route_rx) = mpsc::channel::<RouteTraceMsg>(16);
        let (_allow_tx, allowlist_rx) = watch::channel(Arc::new(HashSet::<IpAddr>::new()));
        let (rate_tx, rate_rx) = watch::channel(TrippyRate {
            protocol: Protocol::Icmp,
            pps: 1.0,
        });
        // Keep the sender alive so the watch channel stays open.
        let _keep_rate = rate_tx;

        let target = meshmon_protocol::Target {
            id: "loopback".into(),
            ip: vec![127, 0, 0, 1].into(),
            display_name: "Loopback".into(),
            location: "Test".into(),
            lat: 0.0,
            lon: 0.0,
            tcp_probe_port: 0,
            udp_probe_port: 0,
        };

        let _join = prober.spawn_target(target, rate_rx, route_tx, allowlist_rx, cancel.clone());

        let mut count = 0usize;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        while tokio::time::Instant::now() < deadline && count < 3 {
            match tokio::time::timeout(std::time::Duration::from_secs(3), route_rx.recv()).await {
                Ok(Some(msg)) => {
                    assert_eq!(msg.protocol, Protocol::Icmp);
                    count += 1;
                }
                Ok(None) => break, // channel closed unexpectedly
                Err(_) => {}       // timeout; keep trying until deadline
            }
        }
        assert!(
            count >= 3,
            "expected >=3 RouteTraceMsgs in 8s on loopback, got {count}"
        );

        cancel.cancel();
        // Give the blocking task up to 15s to exit.
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    }
}
