//! UDP prober pool (spec 02 § UDP prober architecture).
//!
//! One [`UdpProberPool`] per agent. The pool owns:
//! * a shared `UdpSocket` bound to an ephemeral port on 0.0.0.0,
//! * a `DashMap<(IpAddr, u16), Arc<Mutex<TargetState>>>` for per-target
//!   state keyed by `(target_ip, udp_probe_port)` so two agents sharing
//!   an IP (NAT) but listening on distinct ports don't collide,
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

/// Dispatch map keyed by `(target_ip, udp_probe_port)`. The port is the
/// target's listener port — replies always come from there, so the tuple
/// uniquely disambiguates two targets that share an IP (NAT).
type DispatchMap = DashMap<(IpAddr, u16), Arc<Mutex<TargetState>>>;

/// Shared UDP prober. Hold an `Arc<UdpProberPool>` for the lifetime of the
/// agent and `spawn_target` for each target.
pub struct UdpProberPool {
    socket: Arc<UdpSocket>,
    targets: Arc<DispatchMap>,
    secret_rx: watch::Receiver<SecretSnapshot>,
}

impl UdpProberPool {
    /// Build the pool and spawn the receiver + sweeper tasks.
    pub async fn new(
        secret_rx: watch::Receiver<SecretSnapshot>,
        cancel: CancellationToken,
    ) -> std::io::Result<Arc<Self>> {
        let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
        let targets: Arc<DispatchMap> = Arc::new(DashMap::new());

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
        if let Some(prev) = self.targets.insert((ip, port), state.clone()) {
            // Duplicate `spawn_target` for the same (ip, port) without an
            // intervening `forget_if_owner`. The receiver now dispatches
            // to the new state; the old sender (if still running) will
            // eventually exit on cancel. Without this guard its final
            // `forget_if_owner` would correctly leave our new entry in
            // place, but its inflight/sweeper state becomes orphaned —
            // log loudly so operators can investigate the caller.
            tracing::warn!(
                %ip,
                port,
                "udp prober spawn_target called while prior target still active — \
                 orphan dispatch state corrected",
            );
            drop(prev);
        }

        let pool = Arc::clone(self);
        tokio::spawn(async move {
            run_sender(pool, target.id, ip, port, state, rate_rx, cancel).await;
        })
    }

    /// Remove a target from dispatch only if the registered state is still
    /// the one the caller owns. Prevents a lame-duck sender from evicting
    /// its replacement after a late `spawn_target` clobber (see the warn
    /// inside [`UdpProberPool::spawn_target`]).
    fn forget_if_owner(&self, key: (IpAddr, u16), owned: &Arc<Mutex<TargetState>>) {
        self.targets.remove_if(&key, |_k, v| Arc::ptr_eq(v, owned));
    }
}

// ---- receiver ----

async fn run_receiver(
    socket: Arc<UdpSocket>,
    targets: Arc<DispatchMap>,
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
                // The echo/rejection reply always comes from the target's
                // bound `udp_probe_port` (it uses `send_to` on its own
                // listener socket). Keying on `(ip, port)` lets two
                // targets that share an IP (NAT) but listen on different
                // ports route responses to the correct `TargetState`.
                let Some(state_ref) = targets.get(&(peer.ip(), peer.port())) else {
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
    // Hold the target mutex only long enough to mutate state; release it
    // before awaiting `obs_tx.send` so a full observation channel cannot
    // back-pressure into the sender (which grabs this same mutex on every
    // iteration to check `paused_until`).
    let now = tokio::time::Instant::now();
    let (obs_tx, to_send): (mpsc::Sender<ProbeObservation>, Option<ProbeObservation>) = {
        let mut s = state.lock().await;
        match decoded {
            DecodedResponse::Rejection => {
                let first_rejection_in_window = s.paused_until.is_none_or(|u| u < now);
                s.paused_until = Some(now + REJECTION_PAUSE);
                let obs = if first_rejection_in_window {
                    Some(ProbeObservation {
                        protocol: Protocol::Udp,
                        target_id: s.target_id.clone(),
                        outcome: ProbeOutcome::Refused,
                        hops: None,
                        observed_at: now,
                    })
                } else {
                    // Still rejecting; the pause has been extended above.
                    // No duplicate Refused emit inside the same window.
                    None
                };
                (s.obs_tx.clone(), obs)
            }
            DecodedResponse::Echo { nonce } => {
                let obs = s.pending.remove(&nonce).map(|sent_at| {
                    let rtt_micros = sent_at.elapsed().as_micros().min(u32::MAX as u128) as u32;
                    ProbeObservation {
                        protocol: Protocol::Udp,
                        target_id: s.target_id.clone(),
                        outcome: ProbeOutcome::Success { rtt_micros },
                        hops: None,
                        observed_at: sent_at,
                    }
                });
                // else: stale / duplicate / post-timeout — drop silently.
                (s.obs_tx.clone(), obs)
            }
        }
    };
    if let Some(obs) = to_send {
        let _ = obs_tx.send(obs).await;
    }
}

// ---- sweeper ----

async fn run_sweeper(targets: Arc<DispatchMap>, cancel: CancellationToken) {
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
    // Drop the target mutex before awaiting `obs_tx.send`. See the
    // matching comment in `handle_response`: holding the mutex across
    // an async send lets a backed-up obs channel stall the sender loop.
    // Each expired entry's stored `sent_at` is preserved and re-used as
    // `observed_at`, per the `ProbeObservation::observed_at` contract
    // ("monotonic instant when the probe was sent").
    let now = tokio::time::Instant::now();
    let (target_id, obs_tx, expired): (
        String,
        mpsc::Sender<ProbeObservation>,
        Vec<tokio::time::Instant>,
    ) = {
        let mut s = state.lock().await;
        let cutoff = now - PROBE_TIMEOUT;
        let mut expired: Vec<tokio::time::Instant> = Vec::new();
        s.pending.retain(|_nonce, sent_at| {
            if *sent_at < cutoff {
                expired.push(*sent_at);
                false
            } else {
                true
            }
        });
        (s.target_id.clone(), s.obs_tx.clone(), expired)
    };
    for sent_at in expired {
        let obs = ProbeObservation {
            protocol: Protocol::Udp,
            target_id: target_id.clone(),
            outcome: ProbeOutcome::Timeout,
            hops: None,
            observed_at: sent_at,
        };
        let _ = obs_tx.send(obs).await;
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
    // Independent watch receiver for the UDP secret so the pre-config
    // wait can exit the moment the secret is published.
    let mut secret_rx = pool.secret_rx.clone();
    // Clone the observation sender out of state once. We use this directly
    // (rather than re-locking state) for cheap `is_closed` / `closed()`
    // checks and for the error-path send.
    let obs_tx = state.lock().await.obs_tx.clone();
    loop {
        // If the supervisor has detached, stop producing packets. This also
        // covers the "obs channel full and supervisor gone" edge case where
        // the receiver path's send would otherwise silently drop forever.
        if obs_tx.is_closed() {
            break;
        }

        let interval = rate_rx.borrow().next_interval(&mut rng);

        // If paused, sleep until pause expires (or until cancel / rate change
        // / supervisor detach).
        let paused_until = state.lock().await.paused_until;
        if let Some(until) = paused_until {
            let now = tokio::time::Instant::now();
            if until > now {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = obs_tx.closed() => break,
                    r = rate_rx.changed() => {
                        if r.is_err() { break; }
                        continue;
                    }
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
            _ = obs_tx.closed() => break,
            r = rate_rx.changed() => {
                if r.is_err() { break; }
                continue;
            }
            _ = maybe_sleep(interval) => {
                // Lock, allocate nonce, record pending, release lock, send.
                let secret_guard = pool.secret_rx.borrow().current;
                let Some(secret) = secret_guard else {
                    // Pre-config: the secret has not yet been broadcast (or
                    // was cleared). Wait for cancel / supervisor detach /
                    // rate change / secret publish rather than a bare sleep,
                    // so shutdown isn't stalled by up to 250 ms per loop.
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = obs_tx.closed() => break,
                        r = rate_rx.changed() => {
                            if r.is_err() { break; }
                        }
                        r = secret_rx.changed() => {
                            if r.is_err() { break; }
                        }
                        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                    }
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
                    {
                        let mut s = state.lock().await;
                        s.pending.remove(&nonce);
                    }
                    let obs = ProbeObservation {
                        protocol: Protocol::Udp,
                        target_id: target_id.clone(),
                        outcome: ProbeOutcome::Error(format!("send: {e}")),
                        hops: None,
                        observed_at: tokio::time::Instant::now(),
                    };
                    let _ = obs_tx.send(obs).await;
                }
            }
        }
    }
    // Only forget if the dispatch map still points at our state. Guards
    // against a newer `spawn_target` having already installed a
    // replacement for this (ip, port) (see the warn in `spawn_target`).
    pool.forget_if_owner((target_ip, target_port), &state);
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
        // `observed_at` must be the probe's *sent* time, not when the
        // sweeper swept it — downstream stats depend on that contract
        // (`ProbeObservation::observed_at` docstring).
        let age = obs.observed_at.elapsed();
        assert!(
            age >= TD::from_secs(2),
            "expected observed_at ~= sent_at (>=PROBE_TIMEOUT old), got age {age:?}",
        );

        tgt_cancel.cancel();
        pool_cancel.cancel();
        drop(rate_tx);
        drop(sec_tx);
    }

    /// A supervisor that stops draining the obs channel must not be able
    /// to stall the prober's send loop. Before Fix 2/3, `handle_response`
    /// and `sweep_one` held the target mutex across `obs_tx.send().await`,
    /// so a full channel would back-pressure into the sender's
    /// `state.lock().await` calls.
    #[tokio::test]
    async fn full_obs_channel_does_not_stall_sender() {
        let (echo_port, echo_h, echo_cancel) = start_echo_server().await;
        let (sec_tx, sec_rx) = watch::channel(SecretSnapshot {
            current: Some(SECRET),
            previous: None,
        });
        let pool_cancel = CancellationToken::new();
        let pool = UdpProberPool::new(sec_rx, pool_cancel.clone())
            .await
            .unwrap();

        let (rate_tx, rate_rx) = watch::channel(ProbeRate(50.0));
        // Capacity 1: the second unread obs will block any sender that
        // holds the target mutex across `obs_tx.send().await`.
        let (obs_tx, mut obs_rx) = mpsc::channel(1);
        let tgt_cancel = CancellationToken::new();
        pool.spawn_target(
            target("peer", [127, 0, 0, 1], echo_port),
            rate_rx,
            obs_tx,
            tgt_cancel.clone(),
        );

        // Let the sender run while nobody drains obs_rx — this fills the
        // channel and exercises the "send blocks" path in handle_response.
        tokio::time::sleep(TD::from_millis(200)).await;

        // The sender should still be able to reconfigure (rate change
        // needs to grab the target mutex). If it's stuck inside a
        // `handle_response` that's awaiting `obs_tx.send`, this notify
        // never propagates and no new obs ever arrives.
        rate_tx.send(ProbeRate(100.0)).unwrap();

        // Drain one obs to unblock anything queued, then expect another
        // obs to arrive well under the supervisor's shutdown deadline.
        let _ = tokio::time::timeout(TD::from_secs(2), obs_rx.recv())
            .await
            .expect("first obs never arrived");
        let _ = tokio::time::timeout(TD::from_secs(2), obs_rx.recv())
            .await
            .expect("sender was stalled by a full obs channel");

        tgt_cancel.cancel();
        pool_cancel.cancel();
        echo_cancel.cancel();
        drop(rate_tx);
        drop(sec_tx);
        let _ = echo_h.await;
    }

    /// `forget_if_owner` must not evict a newer state that a clobbering
    /// `spawn_target` installed. This exercises the Fix 5 path.
    #[tokio::test]
    async fn forget_if_owner_ignores_foreign_state() {
        let (sec_tx, sec_rx) = watch::channel(SecretSnapshot::default());
        let pool_cancel = CancellationToken::new();
        let pool = UdpProberPool::new(sec_rx, pool_cancel.clone())
            .await
            .unwrap();

        let ip: IpAddr = "127.0.0.50".parse().unwrap();
        let port: u16 = 49_000;
        let key = (ip, port);
        let (old_obs, _old_rx) = mpsc::channel(1);
        let old_state = Arc::new(Mutex::new(TargetState {
            target_id: "old".into(),
            nonce_counter: 0,
            pending: HashMap::new(),
            paused_until: None,
            obs_tx: old_obs,
        }));
        let (new_obs, _new_rx) = mpsc::channel(1);
        let new_state = Arc::new(Mutex::new(TargetState {
            target_id: "new".into(),
            nonce_counter: 0,
            pending: HashMap::new(),
            paused_until: None,
            obs_tx: new_obs,
        }));

        pool.targets.insert(key, new_state.clone());
        // Stale sender's forget must leave `new_state` in place.
        pool.forget_if_owner(key, &old_state);
        assert!(pool.targets.get(&key).is_some(), "foreign forget evicted");

        // Owner's forget does evict.
        pool.forget_if_owner(key, &new_state);
        assert!(
            pool.targets.get(&key).is_none(),
            "owner forget did not evict"
        );

        pool_cancel.cancel();
        drop(sec_tx);
    }

    /// Two targets sharing an IP but on different `udp_probe_port`s must
    /// not collide in the dispatch map. Before the `(ip, port)` key
    /// change, the second `spawn_target` would evict the first and
    /// responses for the first target would be dropped.
    #[tokio::test]
    async fn two_targets_same_ip_different_ports_do_not_collide() {
        let (port_a, h_a, c_a) = start_echo_server().await;
        let (port_b, h_b, c_b) = start_echo_server().await;
        assert_ne!(port_a, port_b);

        let (sec_tx, sec_rx) = watch::channel(SecretSnapshot {
            current: Some(SECRET),
            previous: None,
        });
        let pool_cancel = CancellationToken::new();
        let pool = UdpProberPool::new(sec_rx, pool_cancel.clone())
            .await
            .unwrap();

        let (rate_tx_a, rate_rx_a) = watch::channel(ProbeRate(10.0));
        let (rate_tx_b, rate_rx_b) = watch::channel(ProbeRate(10.0));
        let (obs_tx_a, mut obs_rx_a) = mpsc::channel(16);
        let (obs_tx_b, mut obs_rx_b) = mpsc::channel(16);
        let c_target_a = CancellationToken::new();
        let c_target_b = CancellationToken::new();

        pool.spawn_target(
            target("peer-a", [127, 0, 0, 1], port_a),
            rate_rx_a,
            obs_tx_a,
            c_target_a.clone(),
        );
        pool.spawn_target(
            target("peer-b", [127, 0, 0, 1], port_b),
            rate_rx_b,
            obs_tx_b,
            c_target_b.clone(),
        );

        // Both targets must receive Success observations attributed to
        // their own IDs — proves responses aren't misattributed.
        let obs_a = tokio::time::timeout(TD::from_secs(2), obs_rx_a.recv())
            .await
            .expect("no obs for a")
            .expect("channel a closed");
        let obs_b = tokio::time::timeout(TD::from_secs(2), obs_rx_b.recv())
            .await
            .expect("no obs for b")
            .expect("channel b closed");

        assert_eq!(obs_a.target_id, "peer-a");
        assert_eq!(obs_b.target_id, "peer-b");
        assert!(
            matches!(obs_a.outcome, ProbeOutcome::Success { .. }),
            "{obs_a:?}"
        );
        assert!(
            matches!(obs_b.outcome, ProbeOutcome::Success { .. }),
            "{obs_b:?}"
        );

        c_target_a.cancel();
        c_target_b.cancel();
        pool_cancel.cancel();
        c_a.cancel();
        c_b.cancel();
        drop(rate_tx_a);
        drop(rate_tx_b);
        drop(sec_tx);
        let _ = h_a.await;
        let _ = h_b.await;
    }
}
