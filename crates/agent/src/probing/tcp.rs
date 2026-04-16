//! TCP connect prober.
//!
//! One task per target. Each tick does a single
//! `tokio::net::TcpStream::connect` to `(target.ip, target.tcp_probe_port)`
//! with a connect timeout. Outcome is classified as:
//!
//! * `Success { rtt_micros }` — connect completed
//! * `Timeout`                — connect exceeded deadline
//! * `Refused`                — RST / ICMP unreachable / etc.
//! * `Error(String)`          — anything else (ephemeral-port exhaustion…)
//!
//! On close the stream is set to `SO_LINGER(0)` so the kernel sends RST
//! instead of entering TIME_WAIT — the peer's echo listener does the same
//! on its side, keeping per-probe connection-tracking overhead to zero.

use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use meshmon_protocol::{Protocol, Target};
use rand::rngs::SmallRng;
use rand::SeedableRng;
use socket2::SockRef;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::probing::{ProbeObservation, ProbeOutcome, ProbeRate};

/// Per-probe connect timeout. Any connect that takes longer → `Timeout`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Spawn a TCP prober for a single target.
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
    let port = match u16::try_from(target.tcp_probe_port) {
        Ok(p) if p != 0 => p,
        _ => {
            tracing::error!(
                target_id = %target.id,
                port = target.tcp_probe_port,
                "invalid tcp_probe_port"
            );
            return tokio::spawn(async {});
        }
    };
    tokio::spawn(run(target.id, ip, port, rate_rx, obs_tx, cancel))
}

async fn run(
    target_id: String,
    ip: IpAddr,
    port: u16,
    mut rate_rx: watch::Receiver<ProbeRate>,
    obs_tx: mpsc::Sender<ProbeObservation>,
    cancel: CancellationToken,
) {
    // `ThreadRng` is not `Send`, so seed a `SmallRng` from it and keep that
    // across awaits. Seeding is fine because this rng is only used to jitter
    // probe intervals — no cryptographic requirement.
    let mut rng = SmallRng::from_rng(&mut rand::rng());
    loop {
        let interval = rate_rx.borrow().next_interval(&mut rng);

        tokio::select! {
            _ = cancel.cancelled() => return,
            r = rate_rx.changed() => {
                if r.is_err() {
                    return; // sender dropped = shutdown
                }
                continue;
            }
            _ = maybe_sleep(interval) => {
                let obs = probe_once(&target_id, ip, port).await;
                if obs_tx.send(obs).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn maybe_sleep(interval: Option<Duration>) {
    match interval {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending::<()>().await,
    }
}

async fn probe_once(target_id: &str, ip: IpAddr, port: u16) -> ProbeObservation {
    let addr = SocketAddr::new(ip, port);
    let start = tokio::time::Instant::now();
    let outcome = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => {
            let rtt = start.elapsed().as_micros().min(u32::MAX as u128) as u32;
            // SO_LINGER(0) → RST on drop, no TIME_WAIT.
            let sock = SockRef::from(&stream);
            let _ = sock.set_linger(Some(Duration::ZERO));
            drop(stream);
            ProbeOutcome::Success { rtt_micros: rtt }
        }
        Ok(Err(e)) => classify_connect_error(e),
        Err(_) => ProbeOutcome::Timeout,
    };

    ProbeObservation {
        protocol: Protocol::Tcp,
        target_id: target_id.to_string(),
        outcome,
        hops: None,
        observed_at: start,
    }
}

fn classify_connect_error(e: std::io::Error) -> ProbeOutcome {
    match e.kind() {
        ErrorKind::ConnectionRefused => ProbeOutcome::Refused,
        ErrorKind::TimedOut => ProbeOutcome::Timeout,
        _ => ProbeOutcome::Error(format!("{}: {e}", e.kind())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::TcpListener;

    fn target(id: &str, ip: [u8; 4], port: u32) -> Target {
        Target {
            id: id.to_string(),
            ip: ip.to_vec().into(),
            display_name: id.to_string(),
            location: String::new(),
            lat: 0.0,
            lon: 0.0,
            tcp_probe_port: port,
            udp_probe_port: 0,
        }
    }

    #[tokio::test]
    async fn success_against_loopback_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accept in the background so connect() completes promptly.
        tokio::spawn(async move {
            loop {
                let Ok((s, _)) = listener.accept().await else {
                    break;
                };
                drop(s);
            }
        });

        let t = target("peer", [127, 0, 0, 1], port as u32);
        let (rate_tx, rate_rx) = watch::channel(ProbeRate(5.0)); // 5 pps
        let (obs_tx, mut obs_rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let handle = spawn(t, rate_rx, obs_tx, cancel.clone());

        let got = tokio::time::timeout(Duration::from_secs(2), obs_rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        assert!(
            matches!(got.outcome, ProbeOutcome::Success { .. }),
            "{got:?}"
        );
        cancel.cancel();
        drop(rate_tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn refused_against_closed_port() {
        // Bind then drop to guarantee the port is ephemeral and unused.
        let sock = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = sock.local_addr().unwrap().port();
        drop(sock);

        let t = target("peer", [127, 0, 0, 1], port as u32);
        let (rate_tx, rate_rx) = watch::channel(ProbeRate(5.0));
        let (obs_tx, mut obs_rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let handle = spawn(t, rate_rx, obs_tx, cancel.clone());

        let got = tokio::time::timeout(Duration::from_secs(2), obs_rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        assert!(matches!(got.outcome, ProbeOutcome::Refused), "{got:?}");
        cancel.cancel();
        drop(rate_tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn idle_when_rate_is_zero() {
        let t = target("peer", [127, 0, 0, 1], 1);
        let (rate_tx, rate_rx) = watch::channel(ProbeRate(0.0));
        let (obs_tx, mut obs_rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let handle = spawn(t, rate_rx, obs_tx, cancel.clone());

        let r = tokio::time::timeout(Duration::from_millis(200), obs_rx.recv()).await;
        assert!(r.is_err(), "should stay idle at pps=0");
        cancel.cancel();
        drop(rate_tx);
        handle.await.unwrap();
    }
}
