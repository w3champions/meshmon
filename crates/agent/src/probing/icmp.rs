//! ICMP Echo pinger.
//!
//! Always-on per-target task that fires plain ICMP Echo Requests at the
//! target's IP and awaits Echo Replies. Runs regardless of which protocol
//! is currently primary so the per-protocol state machine always has a
//! sample source for ICMP — otherwise the trippy-driven MTR would stop
//! emitting ICMP observations the moment the primary swings away to TCP
//! or UDP and the state machine would lose the ability to detect ICMP
//! recovery.
//!
//! Uses `surge-ping` (tokio-native ICMP client). Needs `CAP_NET_RAW` or
//! equivalent — same posture as the trippy driver, no new ops work.

use std::net::IpAddr;
use std::time::Duration;

use meshmon_protocol::{Protocol, Target};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use surge_ping::{Client, Config, PingIdentifier, PingSequence, SurgeError, ICMP};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::probing::{ProbeObservation, ProbeOutcome, ProbeRate};

/// Per-probe timeout. Matches the trippy / TCP connect timeouts used
/// elsewhere so all three protocols report `Timeout` on the same
/// wall-clock budget.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Spawn an ICMP pinger for a single target. Returns a `JoinHandle<()>`
/// matching `tcp::spawn`'s shape.
pub fn spawn(
    target: Target,
    rate_rx: watch::Receiver<ProbeRate>,
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
    // `Config::default()` hardcodes `ICMP::V4`, so select the right
    // socket family based on the target's IP. Without this an IPv6
    // target would bind an ICMPv4 socket and every probe would surface
    // as `ProbeOutcome::Error` on send — meshmon is dual-stack (agents
    // bind `[::]`, targets can be v4 or v6), so this is a live path.
    let config = match ip {
        IpAddr::V4(_) => Config::default(),
        IpAddr::V6(_) => Config::builder().kind(ICMP::V6).build(),
    };
    // Each task builds its own `Client`. The client owns a raw ICMP
    // socket + a background dispatcher task; at 50 targets this means
    // 50 raw sockets + 50 dispatchers, which matches the tokio-task
    // budget in spec 02. Sharing a single client across targets is a
    // future optimization (mirror the `UdpProberPool` pattern) but
    // out of scope for T14.
    let client = match Client::new(&config) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(target_id = %target.id, error = %e, "icmp client bind failed");
            return tokio::spawn(async {});
        }
    };
    tokio::spawn(run(target.id, ip, client, rate_rx, obs_tx, cancel))
}

async fn run(
    target_id: String,
    ip: IpAddr,
    client: Client,
    mut rate_rx: watch::Receiver<ProbeRate>,
    obs_tx: mpsc::Sender<ProbeObservation>,
    cancel: CancellationToken,
) {
    let mut rng = SmallRng::from_rng(&mut rand::rng());
    // Random identifier per pinger avoids cross-target reply confusion
    // when two pingers end up on the same process. The dispatcher inside
    // `surge-ping` filters replies by (identifier, sequence).
    let identifier = PingIdentifier(rand::random::<u16>());
    let mut pinger = client.pinger(ip, identifier).await;
    pinger.timeout(PROBE_TIMEOUT);
    let payload = [0u8; 8];

    loop {
        let interval = rate_rx.borrow().next_interval(&mut rng);

        tokio::select! {
            _ = cancel.cancelled() => return,
            r = rate_rx.changed() => {
                if r.is_err() { return; }
                continue;
            }
            _ = async {
                match interval {
                    Some(d) => tokio::time::sleep(d).await,
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }

        // Random sequence per probe avoids cross-probe reply confusion
        // when a delayed reply arrives after the monotonic counter has
        // wrapped (every ~18h at 1 pps). `surge-ping` filters replies by
        // (PingIdentifier, PingSequence), so collisions let a stale reply
        // be mis-attributed to a later probe.
        let sequence = PingSequence(rng.random::<u16>());
        let send_time = Instant::now();
        let outcome = match pinger.ping(sequence, &payload).await {
            Ok((_pkt, rtt)) => ProbeOutcome::Success {
                rtt_micros: rtt.as_micros().min(u128::from(u32::MAX)) as u32,
            },
            Err(SurgeError::Timeout { .. }) => ProbeOutcome::Timeout,
            Err(e) => ProbeOutcome::Error(e.to_string()),
        };

        let obs = ProbeObservation {
            protocol: Protocol::Icmp,
            target_id: target_id.clone(),
            outcome,
            hops: None,
            observed_at: send_time,
        };
        if obs_tx.send(obs).await.is_err() {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_target(id: &str) -> Target {
        Target {
            id: id.to_string(),
            ip: vec![127, 0, 0, 1].into(),
            display_name: format!("Test {id}"),
            location: "Test".to_string(),
            lat: 0.0,
            lon: 0.0,
            tcp_probe_port: 0,
            udp_probe_port: 0,
        }
    }

    /// Loopback ping — requires `CAP_NET_RAW` (or macOS SOCK_DGRAM
    /// ICMP support). On CI without raw-socket permission this test
    /// fails at `Client::new`; `#[ignore]` keeps it opt-in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires CAP_NET_RAW; run on a local dev box with `cargo test -- --ignored`"]
    async fn loopback_icmp_ping_succeeds() {
        let cancel = CancellationToken::new();
        let (_rate_tx, rate_rx) = watch::channel(ProbeRate(10.0));
        let (obs_tx, mut obs_rx) = mpsc::channel::<ProbeObservation>(32);

        let handle = spawn(test_target("loopback"), rate_rx, obs_tx, cancel.clone());

        // Wait for the first observation, or fail after 3 s.
        let obs = tokio::time::timeout(Duration::from_secs(3), obs_rx.recv())
            .await
            .expect("timed out waiting for ICMP observation")
            .expect("observation channel closed");
        assert_eq!(obs.protocol, Protocol::Icmp);
        assert_eq!(obs.target_id, "loopback");
        assert!(
            matches!(obs.outcome, ProbeOutcome::Success { .. }),
            "expected Success outcome on loopback, got {:?}",
            obs.outcome,
        );

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    /// Invalid IP (all zeros — not a meaningful ICMP target). This one
    /// doesn't require raw socket privileges at the client-build level
    /// but still can't be guaranteed on CI; keep `#[ignore]`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires CAP_NET_RAW"]
    async fn invalid_ip_emits_timeout_or_error() {
        let cancel = CancellationToken::new();
        let mut target = test_target("bogus");
        target.ip = vec![0, 0, 0, 0].into(); // 0.0.0.0 — kernel won't route
        let (_rate_tx, rate_rx) = watch::channel(ProbeRate(5.0));
        let (obs_tx, mut obs_rx) = mpsc::channel::<ProbeObservation>(32);

        let handle = spawn(target, rate_rx, obs_tx, cancel.clone());

        let obs = tokio::time::timeout(Duration::from_secs(5), obs_rx.recv())
            .await
            .expect("no observation within 5s")
            .expect("channel closed");
        assert_eq!(obs.protocol, Protocol::Icmp);
        assert!(
            matches!(obs.outcome, ProbeOutcome::Timeout | ProbeOutcome::Error(_)),
            "expected Timeout or Error, got {:?}",
            obs.outcome,
        );

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    /// Cancellation test doesn't actually ping — it only verifies the
    /// task exits promptly on `cancel`. Works without raw-socket
    /// privileges because `Client::new` may still succeed in CI (it's
    /// just that `ping()` would fail); the cancel races the ping.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires CAP_NET_RAW for Client::new on most CI"]
    async fn honors_cancellation() {
        let cancel = CancellationToken::new();
        let (_rate_tx, rate_rx) = watch::channel(ProbeRate(0.0)); // idle rate
        let (obs_tx, _obs_rx) = mpsc::channel::<ProbeObservation>(32);

        let handle = spawn(test_target("cancel"), rate_rx, obs_tx, cancel.clone());
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("icmp task did not honor cancellation")
            .expect("icmp task panicked");
    }
}
