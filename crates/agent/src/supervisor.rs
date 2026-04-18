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
//! * A per-target [`RouteTracker`] that accumulates trippy per-hop
//!   observations (for whichever protocol is currently primary) over a
//!   rolling window sized from `ProbeConfig.primary_window_sec`.
//!
//! Every 10 s the eval tick snapshots per-protocol stats, runs
//! [`TargetStateMachine::evaluate`], publishes new rates via the watch senders,
//! resizes the primary protocol's rolling window on primary swings, and calls
//! [`RouteTracker::reset_for_protocol`] so the next snapshot re-baselines
//! under the new primary.
//!
//! An independent 60 s snapshot tick calls [`RouteTracker::build_snapshot`]
//! followed by [`RouteTracker::diff_against`]; the first snapshot after a
//! reset is emitted unconditionally, and subsequent snapshots only emit
//! when the diff rules from `ProbeConfig.diff_detection` fire. Meaningful
//! snapshots are wrapped in [`RouteSnapshotEnvelope`] (stamped with the
//! supervisor's `target_id`) and pushed into a shared
//! `mpsc::Sender<RouteSnapshotEnvelope>` owned by the
//! [`AgentRuntime`](crate::bootstrap::AgentRuntime) via `try_send`
//! (lossy — a full or closed channel logs and drops). The bootstrap
//! placeholder consumer logs received envelopes at `info`.
//!
//! A separate 60 s metrics tick reads the last-evaluated [`TargetSnapshot`]
//! and emits one [`crate::emitter::PathMetricsMsg`] per protocol where the
//! health is `Some(_)`, pushed into the supervisor → emitter channel via a
//! non-blocking `try_send`. Protocols with `None` health are dropped so the
//! wire payload never carries `ProtocolHealth::Unspecified` (the service
//! rejects that value as `INVALID_ARGUMENT`). Window boundaries are
//! captured from `SystemTime::now()` at tick fire time, never derived from
//! monotonic probe timestamps.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{mpsc, watch, Mutex};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::config::ProbeConfig;
use crate::probing::trippy::TrippyProber;
use crate::probing::udp::UdpProberPool;
use crate::probing::{icmp, tcp, ProbeObservation, ProbeOutcome, ProbeRate, TrippyRate};
use crate::route::{RouteSnapshotEnvelope, RouteTracker};
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
pub struct TargetSnapshot {
    pub icmp_health: Option<ProtoHealth>,
    pub tcp_health: Option<ProtoHealth>,
    pub udp_health: Option<ProtoHealth>,
    pub primary: Option<Protocol>,
    pub path: PathHealthState,
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
    /// supervisor task; read via [`SupervisorHandle::snapshot_state`].
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

    /// Non-blocking read of the last evaluated per-target state.
    ///
    /// Returns `None` when the supervisor's run loop currently holds the
    /// inner lock (write happens once per 10 s eval tick). Callers MUST NOT
    /// rely on always seeing a value — missing a read is cheaper than
    /// pausing the supervisor. Mirrors [`SupervisorHandle::snapshot`]'s
    /// try-lock semantics.
    pub fn snapshot_state(&self) -> Option<TargetSnapshot> {
        self.last_state.try_lock().ok().map(|g| g.clone())
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
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    target: Target,
    config_rx: watch::Receiver<ProbeConfig>,
    allowlist_rx: watch::Receiver<Arc<HashSet<IpAddr>>>,
    udp_pool: Arc<UdpProberPool>,
    trippy_prober: Arc<TrippyProber>,
    parent_cancel: CancellationToken,
    snapshot_tx: mpsc::Sender<RouteSnapshotEnvelope>,
    metrics_tx: mpsc::Sender<crate::emitter::PathMetricsMsg>,
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
        allowlist_rx,
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
        metrics_tx,
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
    // Drained by the 60 s snapshot tick below — each emit is a non-blocking
    // `try_send`; a full channel or closed receiver is logged and dropped
    // (snapshots are lossy by design). The supervisor wraps each snapshot
    // in a [`RouteSnapshotEnvelope`] so the emitter can stamp the eventual
    // `RouteSnapshotRequest` with `target_id` without reverse-lookup.
    snapshot_tx: mpsc::Sender<RouteSnapshotEnvelope>,
    // Drained by the 60 s metrics tick below. Same lossy-`try_send`
    // semantics as `snapshot_tx`: a full channel increments a per-target
    // counter and drops; `Closed` latches a flag so subsequent metrics
    // ticks skip the work entirely.
    metrics_tx: mpsc::Sender<crate::emitter::PathMetricsMsg>,
) {
    tracing::info!(target_id = %target.id, "supervisor started");

    let mut tsm = TargetStateMachine::new();

    let mut eval_interval = tokio::time::interval(Duration::from_secs(10));
    eval_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // T15: separate 60 s cadence for route-snapshot builds. Independent
    // of the eval tick because the diff thresholds themselves already
    // gate emission — we do not need eager reset on config changes the
    // way the rate-eval arm does.
    let mut snapshot_interval = tokio::time::interval(Duration::from_secs(60));
    snapshot_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Once `snapshot_tx.try_send` reports `Closed`, the receiver is gone
    // and every future emit attempt on this channel is wasted work that
    // would also re-log the "closed" message on every 60 s tick. Latch
    // this flag and skip the snapshot-tick body entirely after the first
    // Closed observation.
    let mut snapshot_channel_closed: bool = false;

    // Independent 60 s metrics cadence. Emits one PathMetricsMsg per
    // (target, protocol) where the last-evaluated TargetSnapshot has
    // Some(health) — protocols with None health are skipped to avoid
    // sending ProtocolHealth::Unspecified (server rejects as INVALID_ARGUMENT).
    let mut metrics_interval = tokio::time::interval(Duration::from_secs(60));
    metrics_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Latched once try_send reports Closed — the emitter is gone, further
    // pushes are wasted work. Matches the `snapshot_channel_closed` pattern.
    let mut metrics_channel_closed: bool = false;
    // Per-supervisor running counter of Full-channel drops. Local-only;
    // does NOT feed into agent_metadata.dropped_count (that counter is
    // reserved for emitter-side ring-buffer evictions per proto semantics).
    let mut metrics_dropped_full: u64 = 0;

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

                // T15: first-eval seeding — if the tracker has never been
                // assigned a protocol, adopt the current primary. Only then
                // does `primary_transition.is_some()` drive subsequent resets.
                if route_tracker.protocol().is_none() {
                    if let Some(p) = change.primary {
                        tracing::info!(
                            target_id = %target.id,
                            protocol = ?p,
                            "seeding route tracker with initial primary",
                        );
                        route_tracker.reset_for_protocol(Some(p));
                    }
                } else if change.primary_transition.is_some() {
                    reset_tracker_on_swing(
                        &target.id,
                        &mut route_tracker,
                        change.primary,
                    );
                }

                // T15: keep the tracker window in sync with the primary
                // window config. Cheap even if unchanged.
                route_tracker.set_window(Duration::from_secs(
                    config_snapshot.primary_window_sec as u64,
                ));

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
            _ = snapshot_interval.tick() => {
                if snapshot_channel_closed {
                    continue;
                }
                let now = Instant::now();
                let now_wall = SystemTime::now();
                if let Some(snap) = route_tracker.build_snapshot(now, now_wall) {
                    let should_emit = if route_tracker.last_reported().is_none() {
                        true
                    } else {
                        let thresholds = config_rx.borrow().diff_detection();
                        route_tracker.diff_against(&snap, &thresholds).is_some()
                    };
                    if should_emit {
                        let envelope = RouteSnapshotEnvelope {
                            target_id: target.id.clone(),
                            snapshot: snap.clone(),
                        };
                        match snapshot_tx.try_send(envelope) {
                            Ok(()) => {
                                tracing::debug!(
                                    target_id = %target.id,
                                    protocol = ?snap.protocol,
                                    hops = snap.hops.len(),
                                    "route snapshot emitted",
                                );
                                route_tracker.set_last_reported(snap);
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                tracing::warn!(
                                    target_id = %target.id,
                                    "route snapshot channel full; dropping",
                                );
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                tracing::info!(
                                    target_id = %target.id,
                                    "route snapshot channel closed; stopping emission path",
                                );
                                snapshot_channel_closed = true;
                            }
                        }
                    }
                }
            }
            _ = metrics_interval.tick() => {
                if metrics_channel_closed {
                    continue;
                }
                let now_wall = SystemTime::now();
                let window_end = now_wall;

                // Read per-protocol health from last_state (non-blocking; skip on contention).
                let (icmp_h, tcp_h, udp_h) = {
                    match last_state.try_lock() {
                        Ok(guard) => (guard.icmp_health, guard.tcp_health, guard.udp_health),
                        Err(_) => {
                            tracing::trace!(
                                target_id = %target.id,
                                "last_state contended on metrics tick; skipping",
                            );
                            continue;
                        }
                    }
                };

                for (proto, health) in metrics_protocols(icmp_h, tcp_h, udp_h) {
                    let Some(idx) = protocol_index(proto) else { continue };

                    // summary_with_percentiles needs &mut self (it sorts the sample
                    // buffer for p50/p95/p99). snapshot(Protocol) returns FastSummary
                    // without percentiles, so we reach into the inner mutex here.
                    // try_lock: if the eval tick is currently running, skip this
                    // protocol for this tick.
                    //
                    // Capture the effective window *from the stats instance*
                    // before computing the summary: primary- vs diversity-mode
                    // RollingStats uses different window sizes (`primary_window_sec`
                    // vs `diversity_window_sec`), so we must derive
                    // `window_start` per-protocol rather than assume a fixed
                    // 60 s. The service computes rates against this window,
                    // so a mismatch silently mis-reports probe rates.
                    let (summary, window_start) = match stats[idx].try_lock() {
                        Ok(mut g) => {
                            let window = g.window();
                            (g.summary_with_percentiles(), now_wall - window)
                        }
                        Err(_) => {
                            tracing::trace!(
                                target_id = %target.id,
                                protocol = ?proto,
                                "stats contended on metrics tick; skipping protocol",
                            );
                            continue;
                        }
                    };

                    let msg = crate::emitter::PathMetricsMsg {
                        target_id: target.id.clone(),
                        protocol: proto,
                        window_start,
                        window_end,
                        stats: summary,
                        health,
                    };
                    match metrics_tx.try_send(msg) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            metrics_dropped_full = metrics_dropped_full.saturating_add(1);
                            tracing::warn!(
                                target_id = %target.id,
                                protocol = ?proto,
                                total_dropped_by_this_supervisor = metrics_dropped_full,
                                "path_metrics channel full; dropping (emitter fell behind)",
                            );
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            tracing::info!(
                                target_id = %target.id,
                                "path_metrics channel closed; stopping metrics emission",
                            );
                            metrics_channel_closed = true;
                            break;
                        }
                    }
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

/// Enumerate `(protocol, health)` pairs that the 60 s metrics tick should
/// emit `PathMetricsMsg`s for. Protocols whose classified health is `None`
/// are dropped — the wire payload must never carry
/// `ProtocolHealth::Unspecified` (service rejects as `INVALID_ARGUMENT`).
fn metrics_protocols(
    icmp: Option<ProtoHealth>,
    tcp: Option<ProtoHealth>,
    udp: Option<ProtoHealth>,
) -> impl Iterator<Item = (Protocol, ProtoHealth)> {
    [
        (Protocol::Icmp, icmp),
        (Protocol::Tcp, tcp),
        (Protocol::Udp, udp),
    ]
    .into_iter()
    .filter_map(|(p, h)| h.map(|h| (p, h)))
}

/// Apply a primary-swing to the route tracker. Separated from the eval
/// arm so the tracing call is reviewable in isolation.
fn reset_tracker_on_swing(target_id: &str, tracker: &mut RouteTracker, primary: Option<Protocol>) {
    tracing::info!(
        target_id = %target_id,
        tracker_protocol_before = ?tracker.protocol(),
        tracker_protocol_after = ?primary,
        "route tracker reset on primary swing",
    );
    tracker.reset_for_protocol(primary);
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
        tokio::sync::mpsc::Sender<crate::route::RouteSnapshotEnvelope>,
        tokio::sync::mpsc::Receiver<crate::route::RouteSnapshotEnvelope>,
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

    /// Return both halves of an empty allowlist watch channel.
    ///
    /// Callers MUST bind the sender to a variable (e.g. `_allow_tx`) so it
    /// lives for the duration of the spawned supervisor — dropping the sender
    /// closes the channel, which would let future tests observe a spurious
    /// "closed" state on the receiver side. The `empty_` prefix signals that
    /// no allowlist entries are seeded; tests that need to update the
    /// allowlist mid-run can do so via the returned sender.
    #[allow(clippy::type_complexity)]
    fn empty_allowlist_channel() -> (
        watch::Sender<Arc<HashSet<IpAddr>>>,
        watch::Receiver<Arc<HashSet<IpAddr>>>,
    ) {
        watch::channel(Arc::new(HashSet::new()))
    }

    #[tokio::test]
    async fn supervisor_starts_and_cancels() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(test_config());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;
        let (snapshot_tx, _snapshot_rx) = test_snapshot_tx();
        let (metrics_tx, _metrics_rx) = mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();

        let handle = spawn(
            test_target("test-1"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
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

    use crate::probing::{HopObservation, ProbeOutcome};
    use crate::route::RouteSnapshotEnvelope;
    use crate::stats::FastSummary;
    use std::net::{IpAddr, Ipv4Addr};

    /// Construct a v4 `IpAddr` from four octets.
    fn ipv4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    /// Synthesise a trippy-shaped `ProbeObservation` with hops attached.
    ///
    /// Protocol is `Icmp` because the supervisor's first-eval seeding
    /// elects ICMP as primary, and the route tracker only accepts hops
    /// whose observation protocol matches its current accumulation
    /// protocol. TCP/UDP trippy rounds would be filtered out by
    /// `feed_tracker`'s protocol-match guard.
    fn trippy_obs(target: &str, hops: Vec<HopObservation>) -> ProbeObservation {
        ProbeObservation {
            protocol: Protocol::Icmp,
            target_id: target.to_string(),
            outcome: ProbeOutcome::Success { rtt_micros: 10_000 },
            hops: Some(hops),
            observed_at: tokio::time::Instant::now(),
        }
    }

    /// Build one `HopObservation` with the given 1-indexed position, IP,
    /// and RTT (microseconds).
    fn hop(pos: u8, ip: IpAddr, rtt: u32) -> HopObservation {
        HopObservation {
            position: pos,
            ip: Some(ip),
            rtt_micros: Some(rtt),
        }
    }

    /// Yield the executor enough times for the supervisor task to drain
    /// any pending mpsc observations and process any elapsed ticks. Used
    /// by the paused-clock integration tests after
    /// `tokio::time::advance`.
    async fn yield_many(times: usize) {
        for _ in 0..times {
            tokio::task::yield_now().await;
        }
    }

    /// Non-blocking pull of a single snapshot. Using `try_recv` avoids
    /// interacting with tokio's paused-clock timeout, which under
    /// `start_paused = true` can auto-advance in ways that mask a
    /// "snapshot never sent" bug as a late send. Callers yield the
    /// scheduler first to ensure the supervisor has had a chance to
    /// run.
    fn try_drain_one(
        rx: &mut mpsc::Receiver<RouteSnapshotEnvelope>,
    ) -> Option<RouteSnapshotEnvelope> {
        match rx.try_recv() {
            Ok(env) => Some(env),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => None,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => None,
        }
    }

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
        let (metrics_tx, _metrics_rx) = mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();
        let handle = spawn(
            test_target("test-2"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
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
        let (metrics_tx, _metrics_rx) = mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();
        let handle = spawn(
            test_target("routed"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
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
        let (metrics_tx, _metrics_rx) = mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();
        let handle = spawn(
            test_target("refused"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
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
        let (metrics_tx, _metrics_rx) = mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();
        let handle = spawn(
            test_target("swing-test"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
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
    // Task 8: route snapshot integration tests — drive the supervisor end-to-end
    // via the paused tokio clock.
    // ---------------------------------------------------------------------------

    /// First snapshot after startup must be emitted unconditionally — no
    /// `last_reported` exists yet, so the diff-gate in the supervisor's
    /// 60 s tick falls through to the always-emit branch.
    ///
    /// Flow:
    /// 1. Inject 3 ICMP success observations (without hops) so the state
    ///    machine's `select_primary` MIN_TRANSITION_SAMPLES=3 floor is
    ///    satisfied on the first 10 s eval tick.
    /// 2. Advance 11 s → eval tick fires → seeds tracker with ICMP.
    /// 3. Inject one trippy ICMP round with two hops. `feed_tracker`
    ///    routes these into the tracker because obs protocol matches
    ///    the seeded primary.
    /// 4. Advance past the 60 s snapshot tick. The tracker builds a
    ///    snapshot and the supervisor emits it on the first-ever path.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn supervisor_emits_first_snapshot_unconditionally() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(full_config_with_tight_hysteresis());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;
        let (snapshot_tx, mut snapshot_rx) = test_snapshot_tx();
        let (metrics_tx, _metrics_rx) = mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();

        let handle = spawn(
            test_target("first-snap"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
        );

        tokio::task::yield_now().await;

        // Pre-seed ICMP stats so `select_primary` clears its 3-sample floor
        // on the first eval tick and elects ICMP as primary. Without hops —
        // `feed_tracker` drops these anyway because the tracker has no
        // protocol assigned yet, and we want the tracker's hop buffer
        // empty until AFTER seeding so the assertion below (`hops.len()
        // == 2`) is unambiguous.
        for _ in 0..3 {
            handle
                .observation_tx
                .send(icmp_success("first-snap", 1_000))
                .await
                .expect("seed icmp sample");
        }
        yield_many(10).await;

        // Seed the tracker via the eval tick → first-eval primary adoption.
        tokio::time::advance(Duration::from_secs(11)).await;
        yield_many(10).await;

        // Inject one trippy round with two hops. Protocol is ICMP to match
        // the seeded primary.
        handle
            .observation_tx
            .send(trippy_obs(
                "first-snap",
                vec![
                    hop(1, ipv4(10, 0, 0, 1), 1_000),
                    hop(2, ipv4(10, 0, 0, 2), 2_000),
                ],
            ))
            .await
            .expect("send trippy observation");
        yield_many(10).await;

        // Advance past the 60 s snapshot tick. The first snapshot tick
        // fires at supervisor start (tracker empty → no emit); the next
        // fires 60 s after that. We've already burned 11 s of that
        // budget, so 60 more seconds crosses the boundary.
        tokio::time::advance(Duration::from_secs(60)).await;
        yield_many(20).await;

        let env = try_drain_one(&mut snapshot_rx).expect("first-ever snapshot must emit");
        assert_eq!(env.target_id, "first-snap");
        assert_eq!(env.snapshot.protocol, Protocol::Icmp);
        assert_eq!(env.snapshot.hops.len(), 2);

        parent_cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    /// A steady route — identical trippy rounds over multiple snapshot
    /// ticks — must produce exactly ONE snapshot (the first). Subsequent
    /// builds see no change from `last_reported`, so `diff_against`
    /// returns `None` and the supervisor skips emit.
    ///
    /// Flow:
    /// 1. Advance 11 s to seed the tracker with ICMP.
    /// 2. Loop 12 times: send an identical trippy round and advance 10 s.
    ///    This covers ~120 s of simulated time and spans at least two
    ///    60 s snapshot ticks.
    /// 3. Assert exactly one snapshot was delivered (the first).
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn supervisor_emits_one_snapshot_for_steady_route() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(full_config_with_tight_hysteresis());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;
        let (snapshot_tx, mut snapshot_rx) = test_snapshot_tx();
        let (metrics_tx, _metrics_rx) = mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();

        let handle = spawn(
            test_target("steady"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
        );

        tokio::task::yield_now().await;

        // Seed the tracker with ICMP primary via the first eval tick.
        tokio::time::advance(Duration::from_secs(11)).await;
        yield_many(10).await;

        // Inject identical trippy rounds over ~120 s, crossing at least
        // two 60 s snapshot ticks.
        for _ in 0..12 {
            handle
                .observation_tx
                .send(trippy_obs("steady", vec![hop(1, ipv4(10, 0, 0, 1), 1_000)]))
                .await
                .expect("send");
            tokio::time::advance(Duration::from_secs(10)).await;
            yield_many(5).await;
        }

        // The first snapshot tick must have emitted the baseline snapshot.
        let first = try_drain_one(&mut snapshot_rx).expect("first snapshot must emit");
        assert_eq!(first.target_id, "steady");
        assert_eq!(first.snapshot.hops.len(), 1);
        assert_eq!(first.snapshot.protocol, Protocol::Icmp);

        // No further snapshots: subsequent 60 s ticks see an identical
        // canonical snapshot → `diff_against` returns `None`.
        yield_many(20).await;
        let second = try_drain_one(&mut snapshot_rx);
        assert!(
            second.is_none(),
            "steady route must not emit a second snapshot, got {second:?}",
        );

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

    // ---------------------------------------------------------------------------
    // Snapshot channel closed latch — once `try_send` returns `Closed`, the
    // supervisor must stop rebuilding snapshots and stop re-emitting the
    // "channel closed" log. We assert the supervisor joins cleanly across
    // multiple post-close snapshot ticks (no panic, no hang).
    // ---------------------------------------------------------------------------
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn supervisor_stops_snapshot_emission_after_channel_closed() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(full_config_with_tight_hysteresis());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;

        // Build a snapshot channel and immediately drop the receiver so any
        // `try_send` in the supervisor's snapshot tick will observe `Closed`.
        let (snapshot_tx, snapshot_rx) = mpsc::channel::<RouteSnapshotEnvelope>(1);
        drop(snapshot_rx);
        let (metrics_tx, _metrics_rx) = mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();

        let handle = spawn(
            test_target("closed-snap"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
        );

        tokio::task::yield_now().await;

        // Seed enough ICMP samples so the first eval tick elects ICMP primary
        // and primes the tracker; also inject a trippy round so the tracker
        // actually has hops to snapshot on the 60 s tick. Without hops the
        // build_snapshot short-circuits and `try_send` never runs — we need
        // to exercise the `Closed` branch at least once.
        for _ in 0..3 {
            handle
                .observation_tx
                .send(icmp_success("closed-snap", 1_000))
                .await
                .expect("seed icmp sample");
        }
        yield_many(10).await;
        // First eval tick → seed tracker with ICMP.
        tokio::time::advance(Duration::from_secs(11)).await;
        yield_many(10).await;
        handle
            .observation_tx
            .send(trippy_obs(
                "closed-snap",
                vec![hop(1, ipv4(10, 0, 0, 1), 1_000)],
            ))
            .await
            .expect("send trippy observation");
        yield_many(10).await;

        // First snapshot tick — try_send observes Closed, latches the flag.
        tokio::time::advance(Duration::from_secs(61)).await;
        yield_many(20).await;

        // Two more snapshot ticks — must be fully skipped (no panic, no hang).
        tokio::time::advance(Duration::from_secs(120)).await;
        yield_many(20).await;
        tokio::time::advance(Duration::from_secs(120)).await;
        yield_many(20).await;

        parent_cancel.cancel();
        let join_result = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
        assert!(
            join_result.is_ok(),
            "supervisor must join cleanly after multiple post-close snapshot ticks",
        );
        join_result
            .unwrap()
            .expect("supervisor task must not panic after Closed latch");
    }

    // ---------------------------------------------------------------------------
    // SupervisorHandle::snapshot_state accessor tests.
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn snapshot_state_returns_target_snapshot_after_eval_tick() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(full_config_with_tight_hysteresis());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;
        let (snapshot_tx, _snapshot_rx) = test_snapshot_tx();
        let (metrics_tx, _metrics_rx) = mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();

        let handle = spawn(
            test_target("snapshot-state-test"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
        );

        // Inject enough samples per protocol so the state machine has real
        // health to record on the first eval tick.
        for _ in 0..5 {
            for proto in [Protocol::Icmp, Protocol::Tcp, Protocol::Udp] {
                handle
                    .observation_tx
                    .send(ProbeObservation {
                        protocol: proto,
                        target_id: "snapshot-state-test".to_string(),
                        outcome: ProbeOutcome::Success { rtt_micros: 1_000 },
                        hops: None,
                        observed_at: tokio::time::Instant::now(),
                    })
                    .await
                    .expect("send");
            }
        }
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        // Advance past the first eval tick (10 s).
        tokio::time::advance(Duration::from_secs(11)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        let snap = handle.snapshot_state().expect(
            "snapshot_state should not block and the eval tick has released the lock by now",
        );
        // Exact value may depend on evaluation — the strong invariant is that
        // after at least one eval tick, at least one protocol was classified.
        assert!(
            snap.icmp_health.is_some() || snap.tcp_health.is_some() || snap.udp_health.is_some(),
            "expected at least one protocol to have Some(health) after the first eval tick; snap={snap:?}"
        );

        parent_cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn snapshot_state_returns_none_when_lock_contended() {
        // Build a SupervisorHandle with a synthetic `last_state` we can hold
        // the lock on from the test thread — verifies the accessor never
        // blocks when the lock is contended.
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let last_state = Arc::new(Mutex::new(TargetSnapshot::default()));
        let held = Arc::clone(&last_state);
        let _guard = held.try_lock().expect("lock the state from the test");

        // Minimal handle shim: all we need is `last_state`. The other fields
        // are unused by `snapshot_state`.
        let (observation_tx, _observation_rx) = mpsc::channel(1);
        let stats: Arc<StatsArray> = Arc::new([
            Mutex::new(RollingStats::new(Duration::from_secs(60))),
            Mutex::new(RollingStats::new(Duration::from_secs(60))),
            Mutex::new(RollingStats::new(Duration::from_secs(60))),
        ]);
        // Spawn a do-nothing task so the JoinHandle field is valid.
        let cancel = CancellationToken::new();
        let join = tokio::spawn(async {});

        let handle = SupervisorHandle {
            cancel: cancel.clone(),
            join,
            observation_tx,
            stats,
            prober_joins: Vec::new(),
            last_state: Arc::clone(&last_state),
        };

        assert!(
            handle.snapshot_state().is_none(),
            "snapshot_state must return None while the state lock is held elsewhere"
        );

        drop(_guard);
        assert!(
            handle.snapshot_state().is_some(),
            "after releasing the lock the accessor must return Some(_)"
        );

        cancel.cancel();
        let _ = handle.join.await;
    }

    // -----------------------------------------------------------------------
    // 60 s metrics tick — emits PathMetricsMsg per protocol with
    // Some(health) and drops protocols without a classified health value.
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn supervisor_emits_path_metrics_per_protocol_after_eval() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(full_config_with_tight_hysteresis());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;
        let (snapshot_tx, _snapshot_rx) = test_snapshot_tx();
        let (metrics_tx, mut metrics_rx) =
            tokio::sync::mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();

        let handle = spawn(
            test_target("metrics-tick-test"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
        );

        // Seed each protocol with enough successes so the state machine
        // classifies them on the first eval tick.
        for _ in 0..5 {
            for proto in [Protocol::Icmp, Protocol::Tcp, Protocol::Udp] {
                handle
                    .observation_tx
                    .send(ProbeObservation {
                        protocol: proto,
                        target_id: "metrics-tick-test".to_string(),
                        outcome: ProbeOutcome::Success { rtt_micros: 1_000 },
                        hops: None,
                        observed_at: tokio::time::Instant::now(),
                    })
                    .await
                    .expect("send");
            }
        }
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        // Advance past the first 10 s eval tick so TargetSnapshot is populated.
        tokio::time::advance(Duration::from_secs(11)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        // Advance past the 60 s metrics tick.
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        let mut got = Vec::new();
        while let Ok(msg) = metrics_rx.try_recv() {
            got.push(msg);
        }
        assert!(
            !got.is_empty(),
            "expected >=1 PathMetricsMsg after eval+metrics tick",
        );
        assert!(
            got.iter().all(|m| m.target_id == "metrics-tick-test"),
            "all messages should carry our target_id",
        );

        parent_cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn path_metrics_window_reflects_stats_window() {
        // Regression: the metrics tick previously hard-coded a 60 s window
        // (`window_start = now_wall - Duration::from_secs(60)`) regardless
        // of the underlying RollingStats window. With the tight-hysteresis
        // config (`primary_sec: 300`, `diversity_sec: 900`), every protocol
        // starts on the 900 s diversity window, so the emitted
        // PathMetrics.window_(start|end) pair must span ~900 s — the
        // service computes rates against this span and a 60 s mis-label
        // silently inflates reported rates 15x.
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(full_config_with_tight_hysteresis());
        let (pool, trippy) = build_test_pool(parent_cancel.clone()).await;
        let (snapshot_tx, _snapshot_rx) = test_snapshot_tx();
        let (metrics_tx, mut metrics_rx) =
            tokio::sync::mpsc::channel::<crate::emitter::PathMetricsMsg>(16);
        let (_allow_tx, allowlist_rx) = empty_allowlist_channel();

        let handle = spawn(
            test_target("window-label-test"),
            config_rx,
            allowlist_rx,
            pool,
            trippy,
            parent_cancel.clone(),
            snapshot_tx,
            metrics_tx,
        );

        // Seed every protocol so the state machine classifies all three
        // on the first eval tick — without this, metrics_protocols filters
        // them out and the test passes vacuously with zero observed msgs.
        for _ in 0..5 {
            for proto in [Protocol::Icmp, Protocol::Tcp, Protocol::Udp] {
                handle
                    .observation_tx
                    .send(ProbeObservation {
                        protocol: proto,
                        target_id: "window-label-test".to_string(),
                        outcome: ProbeOutcome::Success { rtt_micros: 1_000 },
                        hops: None,
                        observed_at: tokio::time::Instant::now(),
                    })
                    .await
                    .expect("send");
            }
        }
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        // Pass the first 10 s eval tick so last_state is populated.
        tokio::time::advance(Duration::from_secs(11)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        // Pass the 60 s metrics tick so supervisor emits PathMetricsMsg.
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        let mut got = Vec::new();
        while let Ok(msg) = metrics_rx.try_recv() {
            got.push(msg);
        }
        assert!(
            !got.is_empty(),
            "expected >=1 PathMetricsMsg after eval+metrics tick",
        );

        // For every emitted message, the span window_end - window_start must
        // match the effective RollingStats window for that protocol. With
        // the tight-hysteresis config each protocol uses one of:
        //   - primary window (300 s) if elected primary by the eval tick
        //   - diversity window (900 s) otherwise
        // Either is acceptable; what must never happen is the old 60 s
        // hard-coded span.
        for msg in &got {
            let span = msg
                .window_end
                .duration_since(msg.window_start)
                .expect("window_end >= window_start");
            let secs = span.as_secs();
            assert!(
                secs == 300 || secs == 900,
                "expected window span to be 300 s (primary) or 900 s (diversity), got {secs} s for proto={:?}",
                msg.protocol,
            );
        }

        parent_cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
    }

    #[test]
    fn metrics_protocols_skips_none_and_preserves_order() {
        // Invariant: `None` health entries are dropped so the wire payload
        // can never encode `ProtocolHealth::Unspecified`. Kept pairs
        // preserve the ICMP, TCP, UDP iteration order the metrics tick
        // relies on for reproducible batching.
        let all_none = metrics_protocols(None, None, None).collect::<Vec<_>>();
        assert!(
            all_none.is_empty(),
            "no protocols should emit when every health is None"
        );

        let only_tcp =
            metrics_protocols(None, Some(ProtoHealth::Unhealthy), None).collect::<Vec<_>>();
        assert_eq!(
            only_tcp,
            vec![(Protocol::Tcp, ProtoHealth::Unhealthy)],
            "only the TCP-with-Some pair should survive"
        );

        let icmp_and_udp = metrics_protocols(
            Some(ProtoHealth::Healthy),
            None,
            Some(ProtoHealth::Unhealthy),
        )
        .collect::<Vec<_>>();
        assert_eq!(
            icmp_and_udp,
            vec![
                (Protocol::Icmp, ProtoHealth::Healthy),
                (Protocol::Udp, ProtoHealth::Unhealthy),
            ],
            "ICMP and UDP should emit in input order with TCP's None dropped"
        );

        let all_some = metrics_protocols(
            Some(ProtoHealth::Healthy),
            Some(ProtoHealth::Healthy),
            Some(ProtoHealth::Healthy),
        )
        .collect::<Vec<_>>();
        assert_eq!(all_some.len(), 3, "all three protocols should emit");
    }
}
