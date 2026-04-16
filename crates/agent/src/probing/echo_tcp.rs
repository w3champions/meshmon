//! TCP echo listener.
//!
//! Binds `0.0.0.0:port` and services TCP connect-probes from peer agents.
//! There is no application-level echo — peers measure the TCP handshake
//! itself. We accept the connection and close it immediately with RST
//! (SO_LINGER(0)) to minimize kernel state on both ends.

use socket2::SockRef;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Spawn the TCP echo listener. Returns the `JoinHandle` for the listener
/// task. The task exits when `cancel` is cancelled.
pub fn spawn(port: u16, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(port, cancel))
}

async fn run(port: u16, cancel: CancellationToken) {
    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(
                port,
                error = %e,
                "tcp echo listener failed to bind; TCP probing will not work"
            );
            return;
        }
    };
    tracing::info!(port, "tcp echo listener ready");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("tcp echo listener stopping");
                return;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        // SO_LINGER(0) → RST on drop, no TIME_WAIT buildup.
                        let sock = SockRef::from(&stream);
                        if let Err(e) = sock.set_linger(Some(Duration::ZERO)) {
                            tracing::debug!(error = %e, "set_linger failed (ignored)");
                        }
                        drop(stream);
                        tracing::trace!(%peer, "tcp probe accepted");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tcp echo accept failed");
                        // Backoff a tiny amount to avoid hot-looping on
                        // something like EMFILE.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    #[tokio::test]
    async fn accepts_and_closes_immediately() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cancel = CancellationToken::new();
        let handle = spawn(port, cancel.clone());
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Connect and expect the peer to close immediately. With
        // SO_LINGER(0) the close sends a RST, so the reader may observe
        // either a clean EOF (Ok(0)) or ECONNRESET — both prove the peer
        // closed without application data. Platform-dependent: Linux
        // often returns EOF; macOS raises ECONNRESET.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut buf = [0u8; 1];
        let res = tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf)).await;
        match res.expect("read within 500 ms") {
            Ok(n) => assert_eq!(n, 0, "expected EOF, got {n} bytes"),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                // Expected on macOS when peer closes with RST.
            }
            Err(e) => panic!("unexpected read error: {e}"),
        }

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn cancels_cleanly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cancel = CancellationToken::new();
        let handle = spawn(port, cancel.clone());
        tokio::time::sleep(Duration::from_millis(25)).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("timed out waiting for shutdown")
            .unwrap();
    }
}
