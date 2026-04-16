//! Trippy (MTR) prober.
//!
//! One task per target. Each iteration:
//!
//! 1. Read the current [`TrippyRate`] from the watch channel.
//! 2. Acquire a global [`Semaphore`] permit (caps concurrent raw-socket
//!    tracers across all targets).
//! 3. Run one tracer round under [`tokio::task::spawn_blocking`]; the
//!    permit is held only for the blocking call.
//! 4. Release the permit, emit a path-level + hops [`ProbeObservation`].
//! 5. Sleep `1/pps` with ±20 % jitter, loop.
//!
//! Trippy-core 0.13 is fully synchronous and raw-socket-bound, so each
//! round is a `spawn_blocking` worker: we rebuild a [`Builder`] per round
//! with `max_rounds = Some(1)` and rely on [`Tracer::run`] to block until
//! the single round completes, then read `Tracer::snapshot()` to extract
//! hops and the target RTT. Caching a tracer across rounds is not done
//! because trippy-core's state is owned by the tracer's lifetime and
//! `clear()`ing it between rounds does not save the raw-socket setup cost
//! on every platform; keeping the code structure simple here is the better
//! tradeoff.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use meshmon_protocol::{Protocol, Target};
use rand::rngs::SmallRng;
use rand::SeedableRng;
use tokio::sync::{mpsc, watch, Semaphore};
use tokio_util::sync::CancellationToken;
use trippy_core::{Builder, Port, PortDirection, State};

use crate::probing::{HopObservation, ProbeObservation, ProbeOutcome, TrippyRate};

/// Maximum TTL (hops) the tracer will emit probes for.
const MAX_TTL: u8 = 30;

/// Per-probe read timeout inside a round.
const READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Grace period after the target responds before the round is considered
/// complete (allows a few additional late responses to be collected).
const GRACE_DURATION: Duration = Duration::from_millis(100);

/// Sentinel value indicating a target has not published a TCP/UDP port.
///
/// We require a concrete port for TCP/UDP tracing; if the target doesn't
/// carry one the prober emits an error observation rather than probing a
/// bogus port.
const UNSET_PORT: u16 = 0;

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
        obs_tx: mpsc::Sender<ProbeObservation>,
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
                pool, target_id, ip, tcp_port, udp_port, config_rx, obs_tx, cancel,
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
    obs_tx: mpsc::Sender<ProbeObservation>,
    cancel: CancellationToken,
) {
    // `ThreadRng` is not `Send`; seed a Send-safe SmallRng once. Used only
    // for probe-interval jitter — no cryptographic requirement.
    let mut rng = SmallRng::from_rng(&mut rand::rng());

    loop {
        let snapshot = *config_rx.borrow();
        let interval = if snapshot.pps.is_finite() && snapshot.pps > 0.0 {
            Some(jittered_interval(snapshot.pps, &mut rng))
        } else {
            None
        };

        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = pool.cancel.cancelled() => return,
            r = config_rx.changed() => {
                if r.is_err() {
                    return; // sender dropped = shutdown
                }
                continue;
            }
            _ = maybe_sleep(interval) => {
                if snapshot.protocol == Protocol::Unspecified {
                    // pps>0 with UNSPECIFIED protocol is nonsensical; idle.
                    continue;
                }

                let permit = match pool.semaphore.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => return, // semaphore closed
                };
                let round_target_id = target_id.clone();
                let round_proto = snapshot.protocol;
                let result = tokio::task::spawn_blocking(move || {
                    run_one_round(
                        &round_target_id,
                        target_ip,
                        round_proto,
                        tcp_port,
                        udp_port,
                    )
                })
                .await;
                drop(permit);

                match result {
                    Ok(Ok(obs)) => {
                        if obs_tx.send(obs).await.is_err() {
                            return;
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            target_id = %target_id,
                            protocol = ?snapshot.protocol,
                            error = %e,
                            "trippy round failed"
                        );
                        let obs = ProbeObservation {
                            protocol: snapshot.protocol,
                            target_id: target_id.clone(),
                            outcome: ProbeOutcome::Error(e.to_string()),
                            hops: None,
                            observed_at: tokio::time::Instant::now(),
                        };
                        if obs_tx.send(obs).await.is_err() {
                            return;
                        }
                    }
                    Err(join_err) => {
                        tracing::warn!(%join_err, "trippy blocking task panicked");
                        // Preserve "one round tick → one observation" invariant
                        // so downstream rolling-stats stay aligned with rate.
                        let obs = ProbeObservation {
                            protocol: snapshot.protocol,
                            target_id: target_id.clone(),
                            outcome: ProbeOutcome::Error(format!(
                                "trippy panicked: {join_err}"
                            )),
                            hops: None,
                            observed_at: tokio::time::Instant::now(),
                        };
                        if obs_tx.send(obs).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn jittered_interval(pps: f64, rng: &mut impl rand::Rng) -> Duration {
    let mean = 1.0 / pps;
    let jitter = mean * rng.random_range(-0.2..=0.2);
    Duration::from_secs_f64((mean + jitter).max(0.001))
}

async fn maybe_sleep(interval: Option<Duration>) {
    match interval {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending::<()>().await,
    }
}

/// Run one trippy round synchronously. Callers must wrap this in
/// `spawn_blocking` — trippy-core 0.13 performs raw-socket I/O on the
/// calling thread.
fn run_one_round(
    target_id: &str,
    target_ip: IpAddr,
    protocol: Protocol,
    tcp_port: Option<u16>,
    udp_port: Option<u16>,
) -> Result<ProbeObservation, anyhow::Error> {
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
        .max_rounds(Some(1));

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
        _ => builder,
    };

    let tracer = builder
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
        ProbeOutcome::Timeout
    } else {
        match target_hop.best_ms() {
            Some(ms) => ProbeOutcome::Success {
                rtt_micros: ms_to_micros(ms),
            },
            None => ProbeOutcome::Timeout,
        }
    };

    Ok(ProbeObservation {
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
mod tests {
    use super::*;

    #[test]
    fn jittered_interval_is_bounded() {
        let mut rng = SmallRng::from_rng(&mut rand::rng());
        // 1 pps → mean 1s; ±20 % jitter → [800ms, 1200ms].
        for _ in 0..1000 {
            let d = jittered_interval(1.0, &mut rng);
            assert!(d >= Duration::from_millis(800), "too short: {d:?}");
            assert!(d <= Duration::from_millis(1200), "too long: {d:?}");
        }
    }

    #[test]
    fn jittered_interval_clamps_min() {
        let mut rng = SmallRng::from_rng(&mut rand::rng());
        // 10_000 pps would otherwise yield a 100us interval; the clamp
        // floor is 1ms.
        let d = jittered_interval(10_000.0, &mut rng);
        assert!(d >= Duration::from_millis(1), "below floor: {d:?}");
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
}
