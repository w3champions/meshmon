//! UDP echo listener (spec 02 § Agent echo listeners).
//!
//! Binds `0.0.0.0:port` and services 12-byte probes from peer agents:
//!
//! * len != 12                      → drop silent
//! * secret mismatch (current OR previous) → drop silent
//! * source IP not in allowlist     → send rejection response (0xFFFFFFFF nonce)
//! * else                           → echo verbatim
//!
//! Secret is delivered via a `watch` channel so rotation propagates without
//! restart. Allowlist is a `watch::Receiver<Arc<HashSet<IpAddr>>>` populated
//! by the bootstrap refresh loop.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::probing::wire::{decode_probe, encode_rejection, PACKET_LEN};

/// Secret snapshot delivered to the listener.
#[derive(Debug, Clone, Default)]
pub struct SecretSnapshot {
    pub current: Option<[u8; 8]>,
    pub previous: Option<[u8; 8]>,
}

/// Bind the UDP echo listener and spawn its task. Bind happens eagerly so
/// the caller fails fast (and surfaces the error to the operator) if the
/// port is already in use. The returned `JoinHandle` resolves when `cancel`
/// is cancelled.
pub async fn spawn(
    port: u16,
    secret_rx: watch::Receiver<SecretSnapshot>,
    allowlist_rx: watch::Receiver<Arc<HashSet<IpAddr>>>,
    cancel: CancellationToken,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let socket = UdpSocket::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, "udp echo listener ready");
    Ok(tokio::spawn(run(socket, secret_rx, allowlist_rx, cancel)))
}

async fn run(
    socket: UdpSocket,
    secret_rx: watch::Receiver<SecretSnapshot>,
    allowlist_rx: watch::Receiver<Arc<HashSet<IpAddr>>>,
    cancel: CancellationToken,
) {
    // Small slack above PACKET_LEN so oversized packets are detectable.
    let mut buf = [0u8; 32];
    // One-shot pre-config flag: log the *first* probe that arrives before
    // the secret is published, so operators can distinguish "listener is
    // down" from "listener is up but no secret yet". Subsequent pre-config
    // drops stay silent to avoid scanner floods filling logs.
    let mut pre_config_logged = false;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("udp echo listener stopping");
                return;
            }
            r = socket.recv_from(&mut buf) => {
                let (n, peer) = match r {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "udp echo recv failed");
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        continue;
                    }
                };
                if n != PACKET_LEN {
                    // Oversize / undersize → silent drop. Tracing at trace
                    // so scanner floods don't fill logs.
                    tracing::trace!(bytes = n, %peer, "udp echo: wrong length");
                    continue;
                }

                let secret = secret_rx.borrow().clone();
                let Some(current) = secret.current.as_ref() else {
                    // Pre-GetConfig: drop everything, but surface the first
                    // occurrence so operators notice if bootstrap is stuck.
                    if !pre_config_logged {
                        tracing::warn!(
                            %peer,
                            "udp echo listener received probe before GetConfig completed; dropping until secret is published",
                        );
                        pre_config_logged = true;
                    }
                    continue;
                };
                let packet = &buf[..PACKET_LEN];
                if decode_probe(packet, current, secret.previous.as_ref()).is_none() {
                    // Secret mismatch.
                    continue;
                }

                let allowed = allowlist_rx.borrow().contains(&peer.ip());
                if !allowed {
                    let reject = encode_rejection(current);
                    if let Err(e) = socket.send_to(&reject, peer).await {
                        tracing::debug!(error = %e, %peer, "udp rejection send failed");
                    }
                    continue;
                }

                // Echo verbatim.
                if let Err(e) = socket.send_to(packet, peer).await {
                    tracing::debug!(error = %e, %peer, "udp echo send failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::probing::wire::{decode_response, encode_probe, DecodedResponse};

    const SECRET: [u8; 8] = [10, 20, 30, 40, 50, 60, 70, 80];

    async fn start_listener() -> (u16, CancellationToken) {
        // Pick a free port.
        let probe_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = probe_sock.local_addr().unwrap().port();
        drop(probe_sock);

        let (sec_tx, sec_rx) = watch::channel(SecretSnapshot {
            current: Some(SECRET),
            previous: None,
        });
        std::mem::forget(sec_tx); // keep the channel alive for the test duration
        let (al_tx, al_rx) = watch::channel(Arc::new({
            let mut h = HashSet::new();
            h.insert("127.0.0.1".parse().unwrap());
            h
        }));
        std::mem::forget(al_tx);
        let cancel = CancellationToken::new();
        spawn(port, sec_rx, al_rx, cancel.clone())
            .await
            .expect("bind");
        (port, cancel)
    }

    #[tokio::test]
    async fn echoes_valid_probe() {
        let (port, cancel) = start_listener().await;
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sock.connect(("127.0.0.1", port)).await.unwrap();
        let probe = encode_probe(&SECRET, 777);
        sock.send(&probe).await.unwrap();

        let mut buf = [0u8; 12];
        let n = tokio::time::timeout(Duration::from_secs(1), sock.recv(&mut buf))
            .await
            .expect("no response")
            .unwrap();
        assert_eq!(n, 12);
        assert_eq!(
            decode_response(&buf[..n], &SECRET, None),
            Some(DecodedResponse::Echo { nonce: 777 }),
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn drops_wrong_secret_silently() {
        let (port, cancel) = start_listener().await;
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sock.connect(("127.0.0.1", port)).await.unwrap();
        let probe = encode_probe(&[0u8; 8], 42);
        sock.send(&probe).await.unwrap();

        let mut buf = [0u8; 12];
        let r = tokio::time::timeout(Duration::from_millis(200), sock.recv(&mut buf)).await;
        assert!(r.is_err(), "should time out (silent drop)");
        cancel.cancel();
    }

    #[tokio::test]
    async fn rejects_off_allowlist_source() {
        // Swap allowlist to something that excludes 127.0.0.1 mid-flight.
        let probe_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = probe_sock.local_addr().unwrap().port();
        drop(probe_sock);

        let (sec_tx, sec_rx) = watch::channel(SecretSnapshot {
            current: Some(SECRET),
            previous: None,
        });
        std::mem::forget(sec_tx);
        let (al_tx, al_rx) = watch::channel(Arc::new(HashSet::<IpAddr>::new())); // empty allowlist
        std::mem::forget(al_tx);
        let cancel = CancellationToken::new();
        spawn(port, sec_rx, al_rx, cancel.clone())
            .await
            .expect("bind");

        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sock.connect(("127.0.0.1", port)).await.unwrap();
        let probe = encode_probe(&SECRET, 99);
        sock.send(&probe).await.unwrap();

        let mut buf = [0u8; 12];
        let n = tokio::time::timeout(Duration::from_secs(1), sock.recv(&mut buf))
            .await
            .expect("no response")
            .unwrap();
        assert_eq!(
            decode_response(&buf[..n], &SECRET, None),
            Some(DecodedResponse::Rejection),
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn bind_fails_when_port_in_use() {
        // Hold the port on `0.0.0.0` — same address family the listener
        // binds on — so the collision is unambiguous across platforms.
        let held = UdpSocket::bind(("0.0.0.0", 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();

        let (sec_tx, sec_rx) = watch::channel(SecretSnapshot::default());
        std::mem::forget(sec_tx);
        let (al_tx, al_rx) = watch::channel(Arc::new(HashSet::<IpAddr>::new()));
        std::mem::forget(al_tx);
        let cancel = CancellationToken::new();
        let res = spawn(port, sec_rx, al_rx, cancel).await;
        let err = match res {
            Ok(_) => panic!("expected bind to fail"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        drop(held);
    }
}
