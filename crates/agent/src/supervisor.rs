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

use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::config::ProbeConfig;
use crate::probing::ProbeObservation;
use meshmon_protocol::Target;

/// Handle returned by [`spawn`], giving the caller control over the
/// supervisor's lifetime and a sender for probe observations.
pub struct SupervisorHandle {
    /// Cancel this token to request graceful shutdown of the supervisor.
    pub cancel: CancellationToken,
    /// Join handle for the supervisor's tokio task.
    pub join: tokio::task::JoinHandle<()>,
    /// Sender side of the observation channel. Probers (T12) clone this to
    /// push [`ProbeObservation`]s into the supervisor.
    pub observation_tx: mpsc::Sender<ProbeObservation>,
}

/// Spawn a per-target supervisor task.
///
/// The supervisor runs until `parent_cancel` (or the returned child token) is
/// cancelled. It drains incoming [`ProbeObservation`]s on a 10-second interval
/// and reacts to [`ProbeConfig`] updates.
pub fn spawn(
    target: Target,
    config_rx: watch::Receiver<ProbeConfig>,
    parent_cancel: CancellationToken,
) -> SupervisorHandle {
    let cancel = parent_cancel.child_token();
    let (observation_tx, observation_rx) = mpsc::channel::<ProbeObservation>(256);

    let task_cancel = cancel.clone();
    let join = tokio::spawn(run(target, config_rx, observation_rx, task_cancel));

    SupervisorHandle {
        cancel,
        join,
        observation_tx,
    }
}

/// Main supervisor loop — runs until cancellation.
async fn run(
    target: Target,
    mut config_rx: watch::Receiver<ProbeConfig>,
    mut observation_rx: mpsc::Receiver<ProbeObservation>,
    cancel: CancellationToken,
) {
    tracing::info!(target_id = %target.id, "supervisor started");

    let mut eval_interval = tokio::time::interval(std::time::Duration::from_secs(10));
    eval_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(target_id = %target.id, "shutting down");
                break;
            }
            _ = eval_interval.tick() => {
                let count = drain_pending(&mut observation_rx);
                tracing::debug!(
                    target_id = %target.id,
                    count,
                    "drained pending observations",
                );
            }
            result = config_rx.changed() => {
                if result.is_ok() {
                    tracing::info!(target_id = %target.id, "received config update");
                } else {
                    // Config sender dropped — treat as shutdown signal.
                    tracing::info!(
                        target_id = %target.id,
                        "config channel closed, shutting down",
                    );
                    break;
                }
            }
        }
    }

    // Close the receiver so senders get errors immediately, then drain any
    // remaining observations that arrived before shutdown.
    observation_rx.close();
    let remaining = drain_pending(&mut observation_rx);
    tracing::info!(
        target_id = %target.id,
        remaining,
        "supervisor stopped",
    );
}

/// Drain all currently buffered observations via `try_recv()`, returning the
/// count of messages consumed.
fn drain_pending(rx: &mut mpsc::Receiver<ProbeObservation>) -> usize {
    let mut count = 0;
    while rx.try_recv().is_ok() {
        count += 1;
    }
    count
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

    #[tokio::test]
    async fn supervisor_drains_observations_on_shutdown() {
        let parent_cancel = CancellationToken::new();
        let (_config_tx, config_rx) = watch::channel(test_config());

        let handle = spawn(test_target("test-2"), config_rx, parent_cancel.clone());

        // Send two observations.
        let obs = ProbeObservation {
            protocol: Protocol::Icmp,
            target_id: "test-2".to_string(),
            success: true,
            rtt_micros: Some(1000),
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

        // Request shutdown.
        parent_cancel.cancel();

        // The supervisor should drain remaining observations and exit.
        let result = tokio::time::timeout(Duration::from_secs(2), handle.join).await;
        assert!(
            result.is_ok(),
            "supervisor did not shut down within 2 seconds"
        );
        result.unwrap().expect("supervisor task panicked");
    }
}
