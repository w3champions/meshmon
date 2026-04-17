//! Per-target supervisor — manages probe lifecycle for a single [`Target`].
//!
//! Each active target gets its own supervisor spawned as a tokio task. The
//! supervisor owns:
//!
//! * An `mpsc` channel that probers send [`ProbeObservation`]s into; observations
//!   are routed by [`Protocol`] into the matching per-protocol `RollingStats` slot.
//! * A `watch` receiver carrying the latest [`ProbeConfig`] from the service.
//! * A [`CancellationToken`] derived from the parent token so that global
//!   shutdown propagates automatically down to every prober.
//! * Four `watch` senders (ICMP rate, TCP rate, UDP rate, Trippy rate) that
//!   drive the four probers, which are spawned once per target and reconfigured
//!   via the watch channels — never respawned.
//!
//! Every 10 s the eval tick snapshots per-protocol stats, runs
//! [`TargetStateMachine::evaluate`], publishes new rates via the watch senders,
//! and resizes the primary protocol's rolling window on primary swings.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch, Mutex};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::config::ProbeConfig;
use crate::probing::trippy::TrippyProber;
use crate::probing::udp::UdpProberPool;
use crate::probing::{icmp, tcp, ProbeObservation, ProbeOutcome, ProbeRate, TrippyRate};
use crate::route::{RouteSnapshot, RouteTracker};
use crate::state::{PathHealthState, ProtoHealth, StateChange, TargetStateMachine};
use crate::stats::{FastSummary, RollingStats};
use meshmon_protocol::{Protocol, Target};

/// Number of protocols carried in the per-target stats array. Indexed
/// by [`protocol_index`].
const PROTOCOL_COUNT: usize = 3;

/// Map [`Protocol`] to the stable in-array index used by the supervisor's
/// `RollingStats` array. `Protocol::Unspecified` is returned as `None`.
const fn protocol_index(protocol: Protocol) -> Option<usize> {
    match protocol {
        Protocol::Icmp => Some(0),
        Protocol::Tcp => Some(1),
        Protocol::Udp => Some(2),
        Protocol::Unspecified => None,
    }
}

/// Per-protocol rolling stats, shared between the supervisor's run loop
/// (which writes via `insert` / `purge_old`) and the snapshot accessor
/// (which the T14 state machine and tests use to read `summary_fast`).
type StatsArray = [Mutex<RollingStats>; PROTOCOL_COUNT];

/// Snapshot of the last evaluated target state. Shared between the supervisor
/// run loop (writer) and external callers (readers).
#[derive(Debug, Clone, Default)]
pub(crate) struct TargetSnapshot {
    pub(crate) icmp_health: Option<ProtoHealth>,
    pub(crate) tcp_health: Option<ProtoHealth>,
    pub(crate) udp_health: Option<ProtoHealth>,
    pub(crate) primary: Option<Protocol>,
    pub(crate) path: PathHealthState,
}

/// Handle returned by [`spawn`].
pub struct SupervisorHandle {
    /// Cancel this token to request graceful shutdown of the supervisor.
    pub cancel: CancellationToken,
    /// Join handle for the supervisor's tokio task.
    pub join: tokio::task::JoinHandle<()>,
    /// Sender side of the observation channel. Probers (T12) clone this to
    /// push [`ProbeObservation`]s into the supervisor.
    pub observation_tx: mpsc::Sender<ProbeObservation>,
    /// Shared per-protocol stats, exposed via [`SupervisorHandle::snapshot`].
    /// `pub(crate)` so T14's state machine can reach into the same array
    /// from inside the agent crate; tests use [`SupervisorHandle::snapshot`].
    pub(crate) stats: Arc<StatsArray>,
    /// Join handles for the 4 per-target prober tasks. Private because the
    /// only legitimate consumer is [`SupervisorHandle::await_probers`]; no
    /// outside caller should iterate or mutate the vec directly.
    prober_joins: Vec<tokio::task::JoinHandle<()>>,
    /// Most-recently evaluated state snapshot. Written every eval tick by the
    /// supervisor task; read by tests directly. The future T16 emitter will
    /// add a public accessor when it consumes this — until then rustc's
    /// dead-code lint can't see the test-module read because it fires on
    /// the lib-only pass that gates out `#[cfg(test)]` code.
    #[allow(dead_code)]
    pub(crate) last_state: Arc<Mutex<TargetSnapshot>>,
}

impl SupervisorHandle {
    /// O(1) snapshot of the current per-protocol stats. Returns `None` for
    /// `Protocol::Unspecified` and for transient lock contention with the
    /// supervisor's run loop. Callers tolerate the staleness — the
    /// supervisor evaluates state every 10s anyway.
    ///
    /// Uses `try_lock` so this method never blocks. Crucial design choice:
    /// T14 will call this from sync contexts where `blocking_lock` would
    /// panic on a current-thread runtime. Tests that need fresh data may
    /// poll cheaply (e.g. every 20ms) — the `try_lock` never blocks, so
    /// polling never interferes with the supervisor's run loop.
    pub fn snapshot(&self, protocol: Protocol) -> Option<FastSummary> {
        let idx = protocol_index(protocol)?;
        let guard = self.stats[idx].try_lock().ok()?;
        Some(guard.summary_fast())
    }

    /// Await all 4 prober JoinHandles and log panics.
    ///
    /// Not called in production shutdown — `bootstrap` relies on the
    /// cancel token propagating down each prober's task; dropped
    /// `JoinHandle`s detach but the cancel causes clean exit. This method
    /// exists for tests / integration scenarios that need to observe
    /// prober panics explicitly.
    ///
    /// Cancellation-induced `JoinError`s are ignored (expected at shutdown);
    /// genuine panics surface at `warn`.
    pub async fn await_probers(&mut self) {
        for join in std::mem::take(&mut self.prober_joins) {
            if let Err(e) = join.await {
                if !e.is_cancelled() {
                    tracing::warn!(error = %e, "prober task panicked");
                }
            }
        }
    }
}

/// Spawn a per-target supervisor task.
///
/// The supervisor runs until `parent_cancel` (or the returned child token) is
/// cancelled. It routes incoming [`ProbeObservation`]s into per-protocol
/// `RollingStats`, runs `purge_old` on each stats slot every 10s, evaluates
/// the state machine, publishes rates to the 4 prober watch senders, and
/// reacts to [`ProbeConfig`] updates.
pub fn spawn(
    target: Target,
    config_rx: watch::Receiver<ProbeConfig>,
    udp_pool: Arc<UdpProberPool>,
    trippy_prober: Arc<TrippyProber>,
    parent_cancel: CancellationToken,
    snapshot_tx: mpsc::Sender<RouteSnapshot>,
) -> SupervisorHandle {
    let cancel = parent_cancel.child_token();
    let (observation_tx, observation_rx) = mpsc::channel::<ProbeObservation>(256);

    // Snapshot the initial config to size the windows. The run loop also
    // watches `config_rx` for live updates.
    let initial = config_rx.borrow().clone();
    let initial_window = Duration::from_secs(initial.diversity_window_sec as u64);
    // Per-target route tracker. Sized at the primary window; the supervisor
    // resizes it on config changes / primary swings in Task 7. Starts with
    // `protocol = None` so it silently drops incoming hops until T14 elects
    // a primary and the supervisor calls `reset_for_protocol`.
    let initial_primary_window = Duration::from_secs(initial.primary_window_sec as u64);
    let route_tracker = RouteTracker::new(initial_primary_window);
    // All three protocols start at the diversity window. The eval tick calls
    // `set_window` on whichever protocol it elects as primary.
    let stats: Arc<StatsArray> = Arc::new([
        Mutex::new(RollingStats::new(initial_window)),
        Mutex::new(RollingStats::new(initial_window)),
        Mutex::new(RollingStats::new(initial_window)),
    ]);

    // Create the 4 rate watch channels (ICMP, TCP, UDP, Trippy).
    // Start at idle (zero rate) — the first eval tick will publish real rates.
    let (icmp_rate_tx, icmp_rate_rx) = watch::channel(ProbeRate(0.0));
    let (tcp_rate_tx, tcp_rate_rx) = watch::channel(ProbeRate(0.0));
    let (udp_rate_tx, udp_rate_rx) = watch::channel(ProbeRate(0.0));
    let (trippy_rate_tx, trippy_rate_rx) = watch::channel(TrippyRate::idle());

    // Spawn all 4 probers. ICMP spawn is sync (Batch B), matching TCP's shape.
    let target_for_icmp = target.clone();
    let icmp_join = icmp::spawn(
        target_for_icmp,
        icmp_rate_rx,
        observation_tx.clone(),
        cancel.clone(),
    );

    let target_for_tcp = target.clone();
    let tcp_join = tcp::spawn(
        target_for_tcp,
        tcp_rate_rx,
        observation_tx.clone(),
        cancel.clone(),
    );

    let target_for_udp = target.clone();
    let udp_join = udp_pool.spawn_target(
        target_for_udp,
        udp_rate_rx,
        observation_tx.clone(),
        cancel.clone(),
    );

    let target_for_trippy = target.clone();
    let trippy_join = trippy_prober.spawn_target(
        target_for_trippy,
        trippy_rate_rx,
        observation_tx.clone(),
        cancel.clone(),
    );

    let last_state = Arc::new(Mutex::new(TargetSnapshot::default()));

    let task_cancel = cancel.clone();
    let task_stats = Arc::clone(&stats);
    let task_last_state = Arc::clone(&last_state);
    let join = tokio::spawn(run(
        target,
        config_rx,
        observation_rx,
        task_cancel,
        task_stats,
        icmp_rate_tx,
        tcp_rate_tx,
        udp_rate_tx,
        trippy_rate_tx,
        task_last_state,
        route_tracker,
        snapshot_tx,
    ));

    SupervisorHandle {
        cancel,
        join,
        observation_tx,
        stats,
        prober_joins: vec![icmp_join, tcp_join, udp_join, trippy_join],
        last_state,
    }
}

/// Main supervisor loop — runs until cancellation.
#[allow(clippy::too_many_arguments)]
async fn run(
    target: Target,
    mut config_rx: watch::Receiver<ProbeConfig>,
    mut observation_rx: mpsc::Receiver<ProbeObservation>,
    cancel: CancellationToken,
    stats: Arc<StatsArray>,
    icmp_rate_tx: watch::Sender<ProbeRate>,
    tcp_rate_tx: watch::Sender<ProbeRate>,
    udp_rate_tx: watch::Sender<ProbeRate>,
    trippy_rate_tx: watch::Sender<TrippyRate>,
    last_state: Arc<Mutex<TargetSnapshot>>,
    mut route_tracker: RouteTracker,
    // Held by the run loop; T7 wires the 60 s snapshot tick that actually
    // pushes `RouteSnapshot`s onto this channel. The wiring through
    // bootstrap + supervisor::spawn lands in T6 so T7 only has to add
    // the tick itself.
    snapshot_tx: mpsc::Sender<RouteSnapshot>,
) {
    // Silence the "unused until Task 7" warning without prefixing the
    // binding, so the T7 patch touches only the tick it adds.
    let _ = &snapshot_tx;
    tracing::info!(target_id = %target.id, "supervisor started");

    let mut tsm = TargetStateMachine::new();

    let mut eval_interval = tokio::time::interval(Duration::from_secs(10));
    eval_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(target_id = %target.id, "shutting down");
                break;
            }
            maybe_obs = observation_rx.recv() => {
                match maybe_obs {
                    Some(obs) => {
                        route_observation(&stats, &obs).await;
                        feed_tracker(&mut route_tracker, &obs, Instant::now());
                    }
                    None => {
                        // All probers dropped their senders — no more
                        // observations possible. Exit so we don't spin
                        // on this arm forever.
                        tracing::info!(
                            target_id = %target.id,
                            "observation channel closed, shutting down",
                        );
                        break;
                    }
                }
            }
            _ = eval_interval.tick() => {
                let now = Instant::now();
                // Acquire and release each slot lock independently — no slot
                // is ever held across an `.await` that could acquire another
                // slot, so no deadlock is reachable from any code path.
                let icmp_summary = {
                    let mut s = stats[0].lock().await;
                    s.purge_old(now);
                    s.summary_fast()
                };
                let tcp_summary = {
                    let mut s = stats[1].lock().await;
                    s.purge_old(now);
                    s.summary_fast()
                };
                let udp_summary = {
                    let mut s = stats[2].lock().await;
                    s.purge_old(now);
                    s.summary_fast()
                };

                let config_snapshot = config_rx.borrow().clone();
                let change: StateChange = tsm.evaluate(
                    &config_snapshot,
                    [&icmp_summary, &tcp_summary, &udp_summary],
                    now,
                );

                // Resolve a `FastSummary` for a given protocol. Hoisted out of
                // the primary-transition arm so the per-protocol, path, and
                // primary logs can all include the triggering summary per
                // spec 02: "Every state change is logged at INFO with
                // target_id, the before/after state, and the triggering
                // FastSummary (sample_count, successful, failure_rate)."
                let summary_for = |p: Option<Protocol>| -> Option<&FastSummary> {
                    p.and_then(|p| match p {
                        Protocol::Icmp => Some(&icmp_summary),
                        Protocol::Tcp => Some(&tcp_summary),
                        Protocol::Udp => Some(&udp_summary),
                        Protocol::Unspecified => None,
                    })
                };

                // Log protocol transitions, including failure_rate, sample_count,
                // and successful per spec 02.
                for pt in &change.protocol_transitions {
                    let summary = match pt.protocol {
                        Protocol::Icmp => &icmp_summary,
                        Protocol::Tcp => &tcp_summary,
                        Protocol::Udp => &udp_summary,
                        Protocol::Unspecified => continue,
                    };
                    tracing::info!(
                        target_id = %target.id,
                        protocol = ?pt.protocol,
                        from = ?pt.from,
                        to = ?pt.to,
                        failure_rate = summary.failure_rate,
                        sample_count = summary.sample_count,
                        successful = summary.successful,
                        "per-protocol health changed",
                    );
                }
                // Log path transition, including the current primary and its
                // triggering FastSummary so an operator can correlate
                // degraded/unreachable with which protocol drove the decision
                // and the signal strength behind it.
                if let Some((from, to)) = change.path_transition {
                    tracing::info!(
                        target_id = %target.id,
                        from = ?from,
                        to = ?to,
                        primary = ?change.primary,
                        primary_failure_rate = ?summary_for(change.primary).map(|s| s.failure_rate),
                        primary_sample_count = ?summary_for(change.primary).map(|s| s.sample_count),
                        primary_successful = ?summary_for(change.primary).map(|s| s.successful),
                        "path health changed",
                    );
                }
                // Log primary transition. Per spec 02, include the
                // triggering FastSummary context (failure_rate +
                // sample_count + successful) for BOTH the old and new primary
                // so an operator can correlate the swing with the signals
                // that drove it. `None` fields surface when the primary was
                // or is becoming unset.
                if let Some((from, to)) = change.primary_transition {
                    tracing::info!(
                        target_id = %target.id,
                        from = ?from,
                        to = ?to,
                        from_failure_rate = ?summary_for(from).map(|s| s.failure_rate),
                        from_sample_count = ?summary_for(from).map(|s| s.sample_count),
                        from_successful = ?summary_for(from).map(|s| s.successful),
                        to_failure_rate = ?summary_for(to).map(|s| s.failure_rate),
                        to_sample_count = ?summary_for(to).map(|s| s.sample_count),
                        to_successful = ?summary_for(to).map(|s| s.successful),
                        "primary protocol changed",
                    );
                }

                // Per-tick publish. `watch::Sender::send` does NOT dedupe —
                // it bumps the version and wakes every receiver. The prober
                // loops `continue` on any `changed()` wakeup, which restarts
                // their interval sleep from scratch; left unguarded, a 10s
                // eval tick starves any prober whose interval is > 10s
                // (e.g. 0.05 pps / 20s). Use `send_if_modified` so a no-op
                // publish neither bumps the version nor wakes receivers.
                publish_if_changed(&icmp_rate_tx, ProbeRate(change.rates.icmp_pps));
                publish_if_changed(&tcp_rate_tx, ProbeRate(change.rates.tcp_pps));
                publish_if_changed(&udp_rate_tx, ProbeRate(change.rates.udp_pps));
                publish_if_changed(
                    &trippy_rate_tx,
                    TrippyRate {
                        protocol: change.trippy_protocol,
                        pps: change.trippy_pps,
                    },
                );

                // Resize windows: primary gets the primary window, others get diversity.
                resize_windows(&stats, change.primary, &config_snapshot).await;

                // Update the last-state snapshot.
                {
                    let health = tsm.health_snapshot();
                    let mut snap = last_state.lock().await;
                    snap.icmp_health = Some(health[0].1);
                    snap.tcp_health = Some(health[1].1);
                    snap.udp_health = Some(health[2].1);
                    snap.primary = change.primary;
                    snap.path = change.path;
                }
            }
            result = config_rx.changed() => {
                if result.is_err() {
                    tracing::info!(
                        target_id = %target.id,
                        "config channel closed, shutting down",
                    );
                    break;
                }
                tracing::info!(target_id = %target.id, "received config update");
                // Force the next eval tick to fire immediately so new
                // thresholds / rate rows apply without a 10s lag. Without
                // this an operator-visible config change (e.g. tightening
                // unhealthy_trigger_pct) could take up to the remainder of
                // the current interval to take effect. `reset_immediately`
                // (tokio 1.29+) schedules the next tick now.
                eval_interval.reset_immediately();
            }
        }
    }

    observation_rx.close();
    // Drain any final observations so they're accounted for in the stats
    // before the supervisor exits — useful for tests that race an
    // observation send against shutdown. The route tracker also receives
    // these late observations so any T7 snapshot that fires during
    // shutdown-adjacent activity sees a fully-populated accumulator.
    while let Ok(obs) = observation_rx.try_recv() {
        route_observation(&stats, &obs).await;
        feed_tracker(&mut route_tracker, &obs, Instant::now());
    }
    tracing::info!(target_id = %target.id, "supervisor stopped");
}

/// Resize per-protocol windows based on which protocol is currently primary.
/// Primary gets the primary window; all others get the diversity window.
async fn resize_windows(stats: &StatsArray, primary: Option<Protocol>, config: &ProbeConfig) {
    let primary_window = Duration::from_secs(config.primary_window_sec as u64);
    let diversity_window = Duration::from_secs(config.diversity_window_sec as u64);

    for proto in [Protocol::Icmp, Protocol::Tcp, Protocol::Udp] {
        let Some(idx) = protocol_index(proto) else {
            continue;
        };
        let target_window = if Some(proto) == primary {
            primary_window
        } else {
            diversity_window
        };
        let mut slot = stats[idx].lock().await;
        if slot.window() != target_window {
            slot.set_window(target_window);
        }
    }
}

/// Publish `new` over a rate `watch` channel only when it differs from the
/// current value. Prober loops restart their sleep on every `changed()`
/// wakeup, so a no-op per-tick publish would indefinitely starve probers
/// whose interval exceeds the 10s eval tick. `send_if_modified` both
/// dedupes and skips the version bump, so receivers observe no change.
fn publish_if_changed<T: PartialEq>(tx: &watch::Sender<T>, new: T) {
    tx.send_if_modified(|cur| {
        if *cur == new {
            false
        } else {
            *cur = new;
            true
        }
    });
}

/// Map an inbound observation onto the matching `RollingStats`, applying
/// the protocol-specific filter rules from spec 02 § Probe outcomes.
async fn route_observation(stats: &StatsArray, obs: &ProbeObservation) {
    // Drop UDP `Refused` outcomes — they are the allowlist-rejection
    // backoff signal, not a probe sample. Counting them would corrupt
    // `failure_rate` because the prober pauses for 60s on rejection
    // and the burst arrives as one logical event, not many independent
    // failures. TCP `Refused` (RST) and ICMP-anything-else flow through.
    if matches!(obs.outcome, ProbeOutcome::Refused) && obs.protocol == Protocol::Udp {
        tracing::trace!(target_id = %obs.target_id, "dropping UDP Refused from RollingStats");
        return;
    }
    let Some(idx) = protocol_index(obs.protocol) else {
        tracing::warn!(
            target_id = %obs.target_id,
            protocol = ?obs.protocol,
            "ignoring observation with Unspecified protocol",
        );
        return;
    };
    // Use the receive instant as the sample's window-math timestamp.
    // `obs.observed_at` is the probe's send time and can arrive
    // non-monotonically (e.g. UDP timeout observations are emitted ≥2s
    // after their send), which `RollingStats::purge_old` cannot handle
    // because it relies on a monotonic prefix-pop. See `RollingStats::insert` doc.
    stats[idx].lock().await.insert(obs, Instant::now());
}

/// If `obs` carries hop data and its protocol matches the tracker's
/// current accumulation protocol, push the hops into the route tracker.
/// Observations for non-tracked protocols, observations without hops,
/// and observations received before the tracker has been assigned a
/// protocol are silently ignored.
fn feed_tracker(tracker: &mut RouteTracker, obs: &ProbeObservation, now: Instant) {
    let Some(hops) = obs.hops.as_deref() else {
        return;
    };
    if hops.is_empty() {
        return;
    }
    let Some(tracked) = tracker.protocol() else {
        return;
    };
    if obs.protocol != tracked {
        tracing::trace!(
            target_id = %obs.target_id,
            obs_protocol = ?obs.protocol,
            tracked = ?tracked,
            "dropping hops for non-tracked protocol",
        );
        return;
    }
    tracker.observe(hops, now);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use meshmon_protocol::{ConfigResponse, Protocol};
    use std::time::Duration;

    fn test_target(id: &str) -> Target {
        Target {
            id: id.to_string(),
            ip: vec![127, 0, 0, 1].into(),
            display_name: format!("Test {id}"),
            location: "Test".to_string(),
            lat: 0.0,
            lon: 0.0,
            tcp_probe_port: 3555,
            udp_probe_port: 3552,
        }
    }

    /// Build a snapshot channel for use in supervisor tests. The receiver
    /// is returned so the caller can bind it to a `_snapshot_rx` variable
    /// whose lifetime matches the test body — dropping the receiver
    /// closes the channel, which would cause `try_send` in the (future)
    /// supervisor snapshot tick to fail.
    fn test_snapshot_tx() -> (
        tokio::sync::mpsc::Sender<crate::route::RouteSnapshot>,
        tokio::sync::mpsc::Receiver<crate::route::RouteSnapshot>,
    ) {
        tokio::sync::mpsc::channel(8)
    }

    fn test_config() -> ProbeConfig {
        ProbeConfig::from_proto(ConfigResponse {
            udp_probe_secret: vec![0u8; 8].into(),
            ..Default::default()
        })
        .expect("valid test config")
    }

    /// Build a real `UdpProberPool` + `TrippyProber` for use in supervisor tests.
    async fn build_test_pool(cancel: CancellationToken) -> (Arc<UdpProberPool>, Arc<TrippyProber>) {
        use crate::probing::echo_udp::SecretSnapshot;
        use tokio::sync::watch;

        let (_, sec_rx) = watch::channel(SecretSnapshot::default());
        let pool = UdpProberPool::new(sec_rx, cancel.clone())
            .await
            .expect("udp pool bind");
        let trippy = TrippyProber::new(1, cancel);
        (pool, trippy)
    }

    #[tokio::test]
    async fn supervisor_starts_and_cancels() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(test_config());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;
        let (snapshot_tx, _snapshot_rx) = test_snapshot_tx();

        let handle = spawn(
            test_target("test-1"),
            config_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
        );

        // Give the supervisor a moment to start.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // The task should still be running.
        assert!(
            !handle.join.is_finished(),
            "supervisor should still be running"
        );

        // Request shutdown via the parent token.
        parent_cancel.cancel();

        // The supervisor should terminate within a reasonable window.
        let result = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
        assert!(
            result.is_ok(),
            "supervisor did not shut down within 2 seconds"
        );
        result.unwrap().expect("supervisor task panicked");
    }

    use crate::probing::ProbeOutcome;
    use crate::stats::FastSummary;

    fn icmp_success(target: &str, rtt: u32) -> ProbeObservation {
        ProbeObservation {
            protocol: Protocol::Icmp,
            target_id: target.to_string(),
            outcome: ProbeOutcome::Success { rtt_micros: rtt },
            hops: None,
            observed_at: tokio::time::Instant::now(),
        }
    }

    fn tcp_timeout(target: &str) -> ProbeObservation {
        ProbeObservation {
            protocol: Protocol::Tcp,
            target_id: target.to_string(),
            outcome: ProbeOutcome::Timeout,
            hops: None,
            observed_at: tokio::time::Instant::now(),
        }
    }

    fn udp_refused(target: &str) -> ProbeObservation {
        ProbeObservation {
            protocol: Protocol::Udp,
            target_id: target.to_string(),
            outcome: ProbeOutcome::Refused,
            hops: None,
            observed_at: tokio::time::Instant::now(),
        }
    }

    fn tcp_refused(target: &str) -> ProbeObservation {
        ProbeObservation {
            protocol: Protocol::Tcp,
            target_id: target.to_string(),
            outcome: ProbeOutcome::Refused,
            hops: None,
            observed_at: tokio::time::Instant::now(),
        }
    }

    /// Poll `snapshot(protocol)` until it reports `>= expected` samples
    /// or the deadline elapses. `snapshot` may transiently return `None`
    /// under lock contention with the supervisor's run loop — treat that
    /// as "try again later".
    async fn wait_for_sample_count(
        handle: &SupervisorHandle,
        protocol: Protocol,
        expected: u64,
        deadline: tokio::time::Duration,
    ) -> FastSummary {
        let start = tokio::time::Instant::now();
        let mut last_seen: Option<FastSummary> = None;
        loop {
            if let Some(snap) = handle.snapshot(protocol) {
                last_seen = Some(snap);
                if snap.sample_count >= expected {
                    return snap;
                }
            }
            if tokio::time::Instant::now() - start > deadline {
                panic!(
                    "timed out waiting for {expected} samples on {protocol:?}; last_seen = {last_seen:?}",
                );
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervisor_drains_observations_on_shutdown() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(test_config());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;

        let (snapshot_tx, _snapshot_rx) = test_snapshot_tx();
        let handle = spawn(
            test_target("test-2"),
            config_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
        );

        let obs = ProbeObservation {
            protocol: Protocol::Icmp,
            target_id: "test-2".to_string(),
            outcome: ProbeOutcome::Success { rtt_micros: 1000 },
            hops: None,
            observed_at: tokio::time::Instant::now(),
        };

        handle
            .observation_tx
            .send(obs.clone())
            .await
            .expect("send first observation");
        handle
            .observation_tx
            .send(obs)
            .await
            .expect("send second observation");

        // Wait for both to be routed into the ICMP RollingStats — confirms
        // they were actually consumed by the supervisor before shutdown,
        // not just queued and dropped.
        let icmp = wait_for_sample_count(&handle, Protocol::Icmp, 2, Duration::from_secs(2)).await;
        assert_eq!(icmp.sample_count, 2);
        assert_eq!(icmp.successful, 2);

        // Now cancel and confirm shutdown.
        parent_cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
        assert!(
            result.is_ok(),
            "supervisor did not shut down within 2 seconds"
        );
        result.unwrap().expect("supervisor task panicked");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervisor_routes_by_protocol() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(test_config());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;
        let (snapshot_tx, _snapshot_rx) = test_snapshot_tx();
        let handle = spawn(
            test_target("routed"),
            config_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
        );

        // Send one ICMP success and two TCP timeouts.
        handle
            .observation_tx
            .send(icmp_success("routed", 1_500))
            .await
            .unwrap();
        handle
            .observation_tx
            .send(tcp_timeout("routed"))
            .await
            .unwrap();
        handle
            .observation_tx
            .send(tcp_timeout("routed"))
            .await
            .unwrap();

        let icmp = wait_for_sample_count(&handle, Protocol::Icmp, 1, Duration::from_secs(2)).await;
        assert_eq!(icmp.sample_count, 1);
        assert_eq!(icmp.successful, 1);
        assert_eq!(icmp.failure_rate, 0.0);

        let tcp = wait_for_sample_count(&handle, Protocol::Tcp, 2, Duration::from_secs(2)).await;
        assert_eq!(tcp.sample_count, 2);
        assert_eq!(tcp.successful, 0);
        assert_eq!(tcp.failure_rate, 1.0);

        // UDP got nothing → empty neutral summary. Retry briefly if the
        // snapshot races with the run loop's lock.
        let udp = wait_for_sample_count(&handle, Protocol::Udp, 0, Duration::from_secs(1)).await;
        assert_eq!(udp.sample_count, 0);
        assert_eq!(udp.failure_rate, 0.0);

        parent_cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervisor_drops_udp_refused_but_keeps_tcp_refused() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(test_config());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;
        let (snapshot_tx, _snapshot_rx) = test_snapshot_tx();
        let handle = spawn(
            test_target("refused"),
            config_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
        );

        // UDP Refused: dropped before insert → no sample contribution.
        handle
            .observation_tx
            .send(udp_refused("refused"))
            .await
            .unwrap();
        handle
            .observation_tx
            .send(udp_refused("refused"))
            .await
            .unwrap();
        // TCP Refused: counted as failure.
        handle
            .observation_tx
            .send(tcp_refused("refused"))
            .await
            .unwrap();

        let tcp = wait_for_sample_count(&handle, Protocol::Tcp, 1, Duration::from_secs(2)).await;
        assert_eq!(tcp.sample_count, 1);
        assert_eq!(tcp.successful, 0);
        assert_eq!(tcp.failure_rate, 1.0);

        // Absence check: we have no observable event to await, so we sleep
        // briefly to give the supervisor time to process the two UDP Refused
        // messages (which it will drop). The TCP sample count reaching 1
        // above already confirms the supervisor is actively draining the
        // channel; 50ms is sufficient slack.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let udp = wait_for_sample_count(&handle, Protocol::Udp, 0, Duration::from_secs(1)).await;
        assert_eq!(
            udp.sample_count, 0,
            "UDP Refused must not contribute to stats"
        );

        parent_cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    // ---------------------------------------------------------------------------
    // Task 8: integration test — state machine swings primary after ICMP fails
    // ---------------------------------------------------------------------------

    fn full_config_with_tight_hysteresis() -> ProbeConfig {
        use meshmon_protocol::{
            PathHealth as H, PathHealthThresholds, Protocol as P, ProtocolThresholds, RateEntry,
            Windows,
        };
        let rates = vec![
            RateEntry {
                primary: P::Icmp as i32,
                health: H::Normal as i32,
                icmp_pps: 0.2,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            RateEntry {
                primary: P::Icmp as i32,
                health: H::Degraded as i32,
                icmp_pps: 1.0,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            RateEntry {
                primary: P::Icmp as i32,
                health: H::Unreachable as i32,
                icmp_pps: 1.0,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            RateEntry {
                primary: P::Tcp as i32,
                health: H::Normal as i32,
                icmp_pps: 0.05,
                tcp_pps: 0.2,
                udp_pps: 0.05,
            },
            RateEntry {
                primary: P::Tcp as i32,
                health: H::Degraded as i32,
                icmp_pps: 0.05,
                tcp_pps: 1.0,
                udp_pps: 0.05,
            },
            RateEntry {
                primary: P::Tcp as i32,
                health: H::Unreachable as i32,
                icmp_pps: 0.05,
                tcp_pps: 1.0,
                udp_pps: 0.05,
            },
            RateEntry {
                primary: P::Udp as i32,
                health: H::Normal as i32,
                icmp_pps: 0.05,
                tcp_pps: 0.05,
                udp_pps: 0.2,
            },
            RateEntry {
                primary: P::Udp as i32,
                health: H::Degraded as i32,
                icmp_pps: 0.05,
                tcp_pps: 0.05,
                udp_pps: 1.0,
            },
            RateEntry {
                primary: P::Udp as i32,
                health: H::Unreachable as i32,
                icmp_pps: 0.05,
                tcp_pps: 0.05,
                udp_pps: 1.0,
            },
        ];
        ProbeConfig::from_proto(meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![0u8; 8].into(),
            priority: vec![P::Icmp as i32, P::Tcp as i32, P::Udp as i32],
            rates,
            // Tight 1-second hysteresis: fires within 10s eval intervals.
            icmp_thresholds: Some(ProtocolThresholds {
                unhealthy_trigger_pct: 0.9,
                healthy_recovery_pct: 0.1,
                unhealthy_hysteresis_sec: 1,
                healthy_hysteresis_sec: 1,
            }),
            tcp_thresholds: Some(ProtocolThresholds {
                unhealthy_trigger_pct: 0.9,
                healthy_recovery_pct: 0.1,
                unhealthy_hysteresis_sec: 1,
                healthy_hysteresis_sec: 1,
            }),
            udp_thresholds: Some(ProtocolThresholds {
                unhealthy_trigger_pct: 0.9,
                healthy_recovery_pct: 0.1,
                unhealthy_hysteresis_sec: 1,
                healthy_hysteresis_sec: 1,
            }),
            path_health_thresholds: Some(PathHealthThresholds {
                degraded_trigger_pct: 0.05,
                degraded_trigger_sec: 1,
                degraded_min_samples: 3,
                normal_recovery_pct: 0.02,
                normal_recovery_sec: 1,
            }),
            windows: Some(Windows {
                primary_sec: 300,
                diversity_sec: 900,
            }),
            ..Default::default()
        })
        .expect("valid test config")
    }

    /// Feed synthetic observations directly into the supervisor's obs channel,
    /// then wait for the 10-second eval tick to fire and update `last_state`.
    ///
    /// Uses `start_paused = true` with `tokio::time::advance` so 25 simulated
    /// seconds elapse instantly (two eval ticks), making the test fast and
    /// deterministic.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn supervisor_swings_primary_after_icmp_failures() {
        let parent_cancel = CancellationToken::new();
        let cfg = full_config_with_tight_hysteresis();
        let (config_tx, config_rx) = watch::channel(cfg);
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;

        let (snapshot_tx, _snapshot_rx) = test_snapshot_tx();
        let handle = spawn(
            test_target("swing-test"),
            config_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
        );

        // Yield so the supervisor task actually starts and registers its
        // interval timer before we inject observations.
        tokio::task::yield_now().await;

        // Inject enough ICMP failures to cross MIN_TRANSITION_SAMPLES (3)
        // and well above the 90% unhealthy trigger.
        for _ in 0..10 {
            handle
                .observation_tx
                .send(ProbeObservation {
                    protocol: Protocol::Icmp,
                    target_id: "swing-test".to_string(),
                    outcome: ProbeOutcome::Timeout,
                    hops: None,
                    observed_at: tokio::time::Instant::now(),
                })
                .await
                .expect("send icmp failure");
        }
        // Inject TCP successes so it can be elected primary.
        for _ in 0..10 {
            handle
                .observation_tx
                .send(ProbeObservation {
                    protocol: Protocol::Tcp,
                    target_id: "swing-test".to_string(),
                    outcome: ProbeOutcome::Success { rtt_micros: 1_000 },
                    hops: None,
                    observed_at: tokio::time::Instant::now(),
                })
                .await
                .expect("send tcp success");
        }
        // Inject UDP successes too so the path-level state machine doesn't
        // accidentally flip to Unreachable because every non-primary protocol
        // has zero samples. Plan lines 2150-2163 call this out explicitly.
        for _ in 0..10 {
            handle
                .observation_tx
                .send(ProbeObservation {
                    protocol: Protocol::Udp,
                    target_id: "swing-test".to_string(),
                    outcome: ProbeOutcome::Success { rtt_micros: 1_500 },
                    hops: None,
                    observed_at: tokio::time::Instant::now(),
                })
                .await
                .expect("send udp success");
        }

        // Yield so the supervisor processes all queued observations.
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }

        // Advance time past the first eval tick (10s). With 1s hysteresis,
        // the state machine transitions immediately after crossing the
        // unhealthy trigger.
        tokio::time::advance(Duration::from_secs(11)).await;
        // Yield so the supervisor's eval tick arm fires and runs.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        // Advance past a second eval tick to ensure any pending transitions complete.
        tokio::time::advance(Duration::from_secs(11)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        // Read the last-state snapshot to assert primary swung to TCP.
        let snap = handle.last_state.lock().await.clone();

        // After the eval tick processes 10 ICMP failures (failure_rate=1.0 > 0.9
        // threshold), ICMP must be Unhealthy and TCP (which had 10 successes)
        // must be elected primary.
        assert_eq!(
            snap.icmp_health,
            Some(ProtoHealth::Unhealthy),
            "ICMP should be Unhealthy after 10 consecutive failures; snapshot={snap:?}",
        );
        assert_eq!(
            snap.primary,
            Some(Protocol::Tcp),
            "primary should swing to TCP when ICMP is Unhealthy; snapshot={snap:?}",
        );

        // Keep the config sender alive until assertions are done.
        drop(config_tx);
        parent_cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    // ---------------------------------------------------------------------------
    // C1 regression: per-tick publishes must dedupe so prober sleeps don't
    // reset on no-op rate ticks.
    // ---------------------------------------------------------------------------

    /// Assertion pair pinning the C1 defect: naive `send` always wakes
    /// receivers on identical values; `publish_if_changed` does not.
    ///
    /// A bare `watch::Sender::send(v)` where `*receiver.borrow() == v`
    /// still bumps the channel version, so `changed()` resolves
    /// immediately. The prober loops `continue` on any `changed()`
    /// wakeup, which restarts their interval sleep — left unguarded
    /// this starved any prober whose interval exceeded the 10s eval
    /// tick. `publish_if_changed` uses `send_if_modified` to skip the
    /// version bump when the value is unchanged.
    #[tokio::test]
    async fn publish_if_changed_dedupes_identical_rate_updates() {
        // Control: plain `send` does bump version on identical values.
        let (tx_send, mut rx_send) = watch::channel(ProbeRate(0.05));
        rx_send.mark_unchanged();
        tx_send.send(ProbeRate(0.05)).expect("receiver alive");
        let changed_after_send = tokio::time::timeout(Duration::from_millis(50), rx_send.changed())
            .await
            .expect("watch::send bumps version even on identical values");
        assert!(
            changed_after_send.is_ok(),
            "baseline: plain watch::send must wake receivers on identical values",
        );

        // Fix under test: `publish_if_changed` must NOT wake receivers
        // when the new value equals the current value.
        let (tx_guarded, mut rx_guarded) = watch::channel(ProbeRate(0.05));
        rx_guarded.mark_unchanged();
        publish_if_changed(&tx_guarded, ProbeRate(0.05));
        let changed_after_guarded =
            tokio::time::timeout(Duration::from_millis(50), rx_guarded.changed()).await;
        assert!(
            changed_after_guarded.is_err(),
            "publish_if_changed must not wake receivers on identical values",
        );

        // Sanity: a genuinely different value still wakes receivers.
        publish_if_changed(&tx_guarded, ProbeRate(0.20));
        let changed_on_real_update =
            tokio::time::timeout(Duration::from_millis(50), rx_guarded.changed())
                .await
                .expect("publish_if_changed must still propagate real updates");
        assert!(
            changed_on_real_update.is_ok(),
            "publish_if_changed must wake receivers on differing values",
        );
        assert_eq!(rx_guarded.borrow().0, 0.20);
    }
}
