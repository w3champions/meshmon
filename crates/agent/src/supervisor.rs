//! Per-target supervisor — manages probe lifecycle for a single [`Target`].
//!
//! Each active target gets its own supervisor spawned as a tokio task. The
//! supervisor owns:
//!
//! * An `mpsc` channel that prober tasks (T12) send [`ProbeObservation`]s into.
//! * A `watch` receiver carrying the latest [`ProbeConfig`] from the service.
//! * A [`CancellationToken`] derived from the parent token so that global
//!   shutdown propagates automatically.
//!
//! Real evaluation logic (state machines, metrics emission) arrives in T14.
//! This skeleton drains observations, reacts to config changes, and shuts down
//! cleanly.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch, Mutex};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::config::ProbeConfig;
use crate::probing::{ProbeObservation, ProbeOutcome};
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
}

/// Spawn a per-target supervisor task.
///
/// The supervisor runs until `parent_cancel` (or the returned child token) is
/// cancelled. It routes incoming [`ProbeObservation`]s into per-protocol
/// `RollingStats`, runs `purge_old` on each stats slot every 10s, and reacts
/// to [`ProbeConfig`] updates.
pub fn spawn(
    target: Target,
    config_rx: watch::Receiver<ProbeConfig>,
    parent_cancel: CancellationToken,
) -> SupervisorHandle {
    let cancel = parent_cancel.child_token();
    let (observation_tx, observation_rx) = mpsc::channel::<ProbeObservation>(256);

    // Snapshot the initial config to size the windows. The run loop also
    // watches `config_rx` for live updates.
    let initial = config_rx.borrow().clone();
    let initial_window = Duration::from_secs(initial.diversity_window_sec as u64);
    // All three protocols start at the diversity window. T14 will call
    // `set_window` on whichever protocol it elects as primary.
    let stats: Arc<StatsArray> = Arc::new([
        Mutex::new(RollingStats::new(initial_window)),
        Mutex::new(RollingStats::new(initial_window)),
        Mutex::new(RollingStats::new(initial_window)),
    ]);

    let task_cancel = cancel.clone();
    let task_stats = Arc::clone(&stats);
    let join = tokio::spawn(run(
        target,
        config_rx,
        observation_rx,
        task_cancel,
        task_stats,
    ));

    SupervisorHandle {
        cancel,
        join,
        observation_tx,
        stats,
    }
}

/// Main supervisor loop — runs until cancellation.
async fn run(
    target: Target,
    mut config_rx: watch::Receiver<ProbeConfig>,
    mut observation_rx: mpsc::Receiver<ProbeObservation>,
    cancel: CancellationToken,
    stats: Arc<StatsArray>,
) {
    tracing::info!(target_id = %target.id, "supervisor started");

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
                    Some(obs) => route_observation(&stats, &obs).await,
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
                for slot in stats.iter() {
                    slot.lock().await.purge_old(now);
                }
                // T14 will read `summary_fast` from each slot here and
                // run the state machine. T13 just keeps windows fresh.
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
                // T14 will translate the new config into per-protocol
                // window sizes via `set_window`. T13 deliberately does
                // not preempt T14 here.
            }
        }
    }

    observation_rx.close();
    // Drain any final observations so they're accounted for in the stats
    // before the supervisor exits — useful for tests that race an
    // observation send against shutdown.
    while let Ok(obs) = observation_rx.try_recv() {
        route_observation(&stats, &obs).await;
    }
    tracing::info!(target_id = %target.id, "supervisor stopped");
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

    fn test_config() -> ProbeConfig {
        ProbeConfig::from_proto(ConfigResponse {
            udp_probe_secret: vec![0u8; 8].into(),
            ..Default::default()
        })
        .expect("valid test config")
    }

    #[tokio::test]
    async fn supervisor_starts_and_cancels() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(test_config());

        let handle = spawn(test_target("test-1"), config_rx, parent_cancel.clone());

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

        let handle = spawn(test_target("test-2"), config_rx, parent_cancel.clone());

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
        let handle = spawn(test_target("routed"), config_rx, parent_cancel.clone());

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
        let handle = spawn(test_target("refused"), config_rx, parent_cancel.clone());

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
}
