//! UDP prober pool (spec 02 § UDP prober architecture).
//!
//! One [`UdpProberPool`] per agent. The pool owns:
//! * a shared `UdpSocket` bound to an ephemeral port on 0.0.0.0,
//! * a `DashMap<IpAddr, Arc<Mutex<TargetState>>>` for per-target state,
//! * a background receiver task that decodes responses and dispatches,
//! * a background sweep task that times out pending nonces every 500 ms.
//!
//! Callers attach a target with [`UdpProberPool::spawn_target`], which
//! spawns a per-target sender task feeding into the same socket.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use meshmon_protocol::{Protocol, Target};
use rand::rngs::SmallRng;
use rand::SeedableRng;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::probing::echo_udp::SecretSnapshot;
use crate::probing::wire::{
    decode_response, encode_probe, next_nonce, DecodedResponse, PACKET_LEN,
};
use crate::probing::{ProbeObservation, ProbeOutcome, ProbeRate};

/// Per-probe timeout. Nonces older than this are swept into `Timeout`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Pause duration on `Refused`.
const REJECTION_PAUSE: Duration = Duration::from_secs(60);

/// How often the sweep task scans for expired pending entries.
const SWEEP_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug)]
struct TargetState {
    target_id: String,
    nonce_counter: u32,
    pending: HashMap<u32, tokio::time::Instant>,
    paused_until: Option<tokio::time::Instant>,
    obs_tx: mpsc::Sender<ProbeObservation>,
}

/// Shared UDP prober. Hold an `Arc<UdpProberPool>` for the lifetime of the
/// agent and `spawn_target` for each target.
pub struct UdpProberPool {
    socket: Arc<UdpSocket>,
    targets: Arc<DashMap<IpAddr, Arc<Mutex<TargetState>>>>,
    secret_rx: watch::Receiver<SecretSnapshot>,
}

impl UdpProberPool {
    /// Build the pool and spawn the receiver + sweeper tasks.
    pub async fn new(
        secret_rx: watch::Receiver<SecretSnapshot>,
        cancel: CancellationToken,
    ) -> std::io::Result<Arc<Self>> {
        let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
        let targets: Arc<DashMap<IpAddr, Arc<Mutex<TargetState>>>> = Arc::new(DashMap::new());

        tokio::spawn(run_receiver(
            socket.clone(),
            targets.clone(),
            secret_rx.clone(),
            cancel.clone(),
        ));
        tokio::spawn(run_sweeper(targets.clone(), cancel));

        Ok(Arc::new(Self {
            socket,
            targets,
            secret_rx,
        }))
    }

    /// Spawn a sender for `target`. Returns the sender's join handle. The
    /// caller's `cancel` is cascaded into the sender; cancelling it also
    /// removes the target from the dispatch map.
    pub fn spawn_target(
        self: &Arc<Self>,
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
        let port = match u16::try_from(target.udp_probe_port) {
            Ok(p) if p != 0 => p,
            _ => {
                tracing::error!(
                    target_id = %target.id,
                    port = target.udp_probe_port,
                    "invalid udp_probe_port"
                );
                return tokio::spawn(async {});
            }
        };

        let state = Arc::new(Mutex::new(TargetState {
            target_id: target.id.clone(),
            nonce_counter: 0,
            pending: HashMap::new(),
            paused_until: None,
            obs_tx,
        }));
        self.targets.insert(ip, state.clone());

        let pool = Arc::clone(self);
        tokio::spawn(async move {
            run_sender(pool, target.id, ip, port, state, rate_rx, cancel).await;
        })
    }

    /// Remove a target from dispatch (called when its supervisor is torn
    /// down in the refresh loop).
    pub fn forget(&self, ip: &IpAddr) {
        self.targets.remove(ip);
    }
}

// ---- receiver ----

async fn run_receiver(
    socket: Arc<UdpSocket>,
    targets: Arc<DashMap<IpAddr, Arc<Mutex<TargetState>>>>,
    secret_rx: watch::Receiver<SecretSnapshot>,
    cancel: CancellationToken,
) {
    let mut buf = [0u8; 32];
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            r = socket.recv_from(&mut buf) => {
                let (n, peer) = match r {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "udp prober recv failed");
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                };
                if n != PACKET_LEN {
                    continue;
                }
                let secret = secret_rx.borrow().clone();
                let Some(current) = secret.current else { continue };
                let decoded = decode_response(&buf[..PACKET_LEN], &current, secret.previous.as_ref());
                let Some(decoded) = decoded else { continue };
                let Some(state_ref) = targets.get(&peer.ip()) else {
                    continue;
                };
                let state = state_ref.clone();
                drop(state_ref);
                handle_response(state, peer.ip(), decoded).await;
            }
        }
    }
}

async fn handle_response(
    state: Arc<Mutex<TargetState>>,
    _peer_ip: IpAddr,
    decoded: DecodedResponse,
) {
    let mut s = state.lock().await;
    match decoded {
        DecodedResponse::Rejection => {
            // Latch: only emit Refused the first time per pause window.
            let now = tokio::time::Instant::now();
            if s.paused_until.is_none_or(|u| u < now) {
                s.paused_until = Some(now + REJECTION_PAUSE);
                let obs = ProbeObservation {
                    protocol: Protocol::Udp,
                    target_id: s.target_id.clone(),
                    outcome: ProbeOutcome::Refused,
                    hops: None,
                    observed_at: now,
                };
                let _ = s.obs_tx.send(obs).await;
            } else {
                // Extend the pause (target still rejecting us).
                s.paused_until = Some(now + REJECTION_PAUSE);
            }
        }
        DecodedResponse::Echo { nonce } => {
            if let Some(sent_at) = s.pending.remove(&nonce) {
                let rtt_micros = sent_at.elapsed().as_micros().min(u32::MAX as u128) as u32;
                let obs = ProbeObservation {
                    protocol: Protocol::Udp,
                    target_id: s.target_id.clone(),
                    outcome: ProbeOutcome::Success { rtt_micros },
                    hops: None,
                    observed_at: sent_at,
                };
                let _ = s.obs_tx.send(obs).await;
            }
            // else: stale / duplicate / post-timeout — drop silently.
        }
    }
}

// ---- sweeper ----

async fn run_sweeper(
    targets: Arc<DashMap<IpAddr, Arc<Mutex<TargetState>>>>,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = ticker.tick() => {
                let entries: Vec<Arc<Mutex<TargetState>>> =
                    targets.iter().map(|e| e.value().clone()).collect();
                for state in entries {
                    sweep_one(state).await;
                }
            }
        }
    }
}

async fn sweep_one(state: Arc<Mutex<TargetState>>) {
    let now = tokio::time::Instant::now();
    let mut s = state.lock().await;
    let expired: Vec<u32> = s
        .pending
        .iter()
        .filter_map(|(nonce, sent)| (now.duration_since(*sent) > PROBE_TIMEOUT).then_some(*nonce))
        .collect();
    for nonce in expired {
        s.pending.remove(&nonce);
        let obs = ProbeObservation {
            protocol: Protocol::Udp,
            target_id: s.target_id.clone(),
            outcome: ProbeOutcome::Timeout,
            hops: None,
            observed_at: now,
        };
        let _ = s.obs_tx.send(obs).await;
    }
}

// ---- sender ----

async fn run_sender(
    pool: Arc<UdpProberPool>,
    target_id: String,
    target_ip: IpAddr,
    target_port: u16,
    state: Arc<Mutex<TargetState>>,
    mut rate_rx: watch::Receiver<ProbeRate>,
    cancel: CancellationToken,
) {
    let target_addr = std::net::SocketAddr::new(target_ip, target_port);
    // `ThreadRng` is !Send; spawned futures must be Send, so use SmallRng.
    let mut rng = SmallRng::from_rng(&mut rand::rng());
    loop {
        let interval = rate_rx.borrow().next_interval(&mut rng);

        // If paused, sleep until pause expires (or until cancel / rate change).
        let paused_until = state.lock().await.paused_until;
        if let Some(until) = paused_until {
            let now = tokio::time::Instant::now();
            if until > now {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = rate_rx.changed() => continue,
                    _ = tokio::time::sleep_until(until) => {
                        state.lock().await.paused_until = None;
                        continue;
                    }
                }
            } else {
                state.lock().await.paused_until = None;
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => break,
            r = rate_rx.changed() => {
                if r.is_err() { break; }
                continue;
            }
            _ = maybe_sleep(interval) => {
                // Lock, allocate nonce, record pending, release lock, send.
                let secret_guard = pool.secret_rx.borrow().current;
                let Some(secret) = secret_guard else {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                };
                let nonce = {
                    let mut s = state.lock().await;
                    s.nonce_counter = next_nonce(s.nonce_counter);
                    let nonce = s.nonce_counter;
                    s.pending.insert(nonce, tokio::time::Instant::now());
                    nonce
                };
                let packet = encode_probe(&secret, nonce);
                if let Err(e) = pool.socket.send_to(&packet, target_addr).await {
                    let mut s = state.lock().await;
                    s.pending.remove(&nonce);
                    let obs = ProbeObservation {
                        protocol: Protocol::Udp,
                        target_id: target_id.clone(),
                        outcome: ProbeOutcome::Error(format!("send: {e}")),
                        hops: None,
                        observed_at: tokio::time::Instant::now(),
                    };
                    let _ = s.obs_tx.send(obs).await;
                }
            }
        }
    }
    pool.forget(&target_ip);
}

async fn maybe_sleep(interval: Option<Duration>) {
    match interval {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probing::wire::{decode_probe, encode_rejection};
    use tokio::time::Duration as TD;

    const SECRET: [u8; 8] = [5, 6, 7, 8, 9, 10, 11, 12];

    fn target(id: &str, ip: [u8; 4], port: u16) -> Target {
        Target {
            id: id.to_string(),
            ip: ip.to_vec().into(),
            display_name: id.to_string(),
            location: String::new(),
            lat: 0.0,
            lon: 0.0,
            tcp_probe_port: 0,
            udp_probe_port: port as u32,
        }
    }

    async fn start_echo_server() -> (u16, tokio::task::JoinHandle<()>, CancellationToken) {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let port = sock.local_addr().unwrap().port();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 32];
            loop {
                tokio::select! {
                    _ = c2.cancelled() => return,
                    r = sock.recv_from(&mut buf) => {
                        let Ok((n, peer)) = r else { continue };
                        if n == PACKET_LEN {
                            let _ = sock.send_to(&buf[..n], peer).await;
                        }
                    }
                }
            }
        });
        (port, handle, cancel)
    }

    async fn start_rejection_server() -> (u16, tokio::task::JoinHandle<()>, CancellationToken) {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let port = sock.local_addr().unwrap().port();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 32];
            loop {
                tokio::select! {
                    _ = c2.cancelled() => return,
                    r = sock.recv_from(&mut buf) => {
                        let Ok((n, peer)) = r else { continue };
                        if n == PACKET_LEN && decode_probe(&buf[..n], &SECRET, None).is_some() {
                            let _ = sock.send_to(&encode_rejection(&SECRET), peer).await;
                        }
                    }
                }
            }
        });
        (port, handle, cancel)
    }

    #[tokio::test]
    async fn happy_path_echo() {
        let (echo_port, echo_h, echo_cancel) = start_echo_server().await;
        let (sec_tx, sec_rx) = watch::channel(SecretSnapshot {
            current: Some(SECRET),
            previous: None,
        });
        let pool_cancel = CancellationToken::new();
        let pool = UdpProberPool::new(sec_rx, pool_cancel.clone())
            .await
            .unwrap();

        let (rate_tx, rate_rx) = watch::channel(ProbeRate(10.0));
        let (obs_tx, mut obs_rx) = mpsc::channel(16);
        let tgt_cancel = CancellationToken::new();
        pool.spawn_target(
            target("peer", [127, 0, 0, 1], echo_port),
            rate_rx,
            obs_tx,
            tgt_cancel.clone(),
        );

        let obs = tokio::time::timeout(TD::from_secs(2), obs_rx.recv())
            .await
            .expect("no obs")
            .expect("channel");
        assert!(
            matches!(obs.outcome, ProbeOutcome::Success { .. }),
            "{obs:?}"
        );

        tgt_cancel.cancel();
        pool_cancel.cancel();
        echo_cancel.cancel();
        drop(rate_tx);
        drop(sec_tx);
        let _ = echo_h.await;
    }

    #[tokio::test]
    async fn rejection_latches_and_pauses() {
        let (rej_port, rej_h, rej_cancel) = start_rejection_server().await;
        let (sec_tx, sec_rx) = watch::channel(SecretSnapshot {
            current: Some(SECRET),
            previous: None,
        });
        let pool_cancel = CancellationToken::new();
        let pool = UdpProberPool::new(sec_rx, pool_cancel.clone())
            .await
            .unwrap();

        let (rate_tx, rate_rx) = watch::channel(ProbeRate(20.0));
        let (obs_tx, mut obs_rx) = mpsc::channel(16);
        let tgt_cancel = CancellationToken::new();
        pool.spawn_target(
            target("peer", [127, 0, 0, 1], rej_port),
            rate_rx,
            obs_tx,
            tgt_cancel.clone(),
        );

        let obs = tokio::time::timeout(TD::from_secs(2), obs_rx.recv())
            .await
            .expect("no obs")
            .expect("channel");
        assert!(matches!(obs.outcome, ProbeOutcome::Refused), "{obs:?}");

        let r = tokio::time::timeout(TD::from_millis(500), obs_rx.recv()).await;
        assert!(r.is_err(), "should be silent during pause window");

        tgt_cancel.cancel();
        pool_cancel.cancel();
        rej_cancel.cancel();
        drop(rate_tx);
        drop(sec_tx);
        let _ = rej_h.await;
    }

    #[tokio::test]
    async fn timeout_when_no_one_responds() {
        let dead = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);

        let (sec_tx, sec_rx) = watch::channel(SecretSnapshot {
            current: Some(SECRET),
            previous: None,
        });
        let pool_cancel = CancellationToken::new();
        let pool = UdpProberPool::new(sec_rx, pool_cancel.clone())
            .await
            .unwrap();

        let (rate_tx, rate_rx) = watch::channel(ProbeRate(10.0));
        let (obs_tx, mut obs_rx) = mpsc::channel(16);
        let tgt_cancel = CancellationToken::new();
        pool.spawn_target(
            target("peer", [127, 0, 0, 1], dead_port),
            rate_rx,
            obs_tx,
            tgt_cancel.clone(),
        );

        let obs = tokio::time::timeout(TD::from_secs(4), obs_rx.recv())
            .await
            .expect("no obs")
            .expect("channel");
        assert!(matches!(obs.outcome, ProbeOutcome::Timeout), "{obs:?}");

        tgt_cancel.cancel();
        pool_cancel.cancel();
        drop(rate_tx);
        drop(sec_tx);
    }
}
