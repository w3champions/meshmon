//! TCP echo listener.
//!
//! Binds `[::]:port` dual-stack (`IPV6_V6ONLY=false`) and services TCP
//! connect-probes from peer agents over both IPv4 and IPv6. IPv4 peers
//! reach the listener transparently via IPv4-mapped IPv6 addresses
//! (`::ffff:a.b.c.d`). There is no application-level echo — peers measure
//! the TCP handshake itself. We accept the connection and close it
//! immediately with RST (SO_LINGER(0)) to minimize kernel state on both
//! ends.

use socket2::{Domain, SockRef, Socket, Type};
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Backlog passed to `listen()` on the dual-stack socket. Matches the
/// default `tokio::net::TcpListener::bind` behavior (tokio uses 1024 on
/// Linux, 128 on macOS via the libc `SOMAXCONN`); a fixed `128` is a safe
/// lower bound that the kernel clamps to the platform cap if larger.
const TCP_LISTEN_BACKLOG: i32 = 128;

/// Bind the TCP echo listener and spawn its task. Bind happens eagerly so
/// the caller fails fast (and surfaces the error to the operator) if the
/// port is already in use. The returned `JoinHandle` resolves when `cancel`
/// is cancelled.
pub async fn spawn(
    port: u16,
    cancel: CancellationToken,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let listener = bind_dual_stack(port)?;
    tracing::info!(port, "tcp echo listener ready (dual-stack)");
    Ok(tokio::spawn(run(listener, cancel)))
}

/// Build a dual-stack (`IPV6_V6ONLY=false`) TCP listener bound to
/// `[::]:port`. A plain `tokio::net::TcpListener::bind("[::]:port")` would
/// either inherit the kernel's default `IPV6_V6ONLY` (which is `true` on
/// some Linux distributions) or force IPv4-only binds; using `socket2`
/// explicitly clears the flag so IPv4 peers reach us on the same socket.
fn bind_dual_stack(port: u16) -> std::io::Result<TcpListener> {
    let addr = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0);
    let socket = Socket::new(Domain::IPV6, Type::STREAM, None)?;
    socket.set_only_v6(false)?;
    // Required for conversion into a tokio listener.
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::V6(addr).into())?;
    socket.listen(TCP_LISTEN_BACKLOG)?;
    let std_listener: std::net::TcpListener = socket.into();
    TcpListener::from_std(std_listener)
}

async fn run(listener: TcpListener, cancel: CancellationToken) {
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
        let handle = spawn(port, cancel.clone()).await.expect("bind");

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
        let handle = spawn(port, cancel.clone()).await.expect("bind");
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("timed out waiting for shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn bind_fails_when_port_in_use() {
        // Hold the port on `[::]` — same dual-stack address the listener
        // binds on — so the collision is unambiguous across platforms.
        // Binding a v4 squatter on `0.0.0.0` is a weaker test: Linux and
        // macOS *do* refuse the subsequent dual-stack `[::]` bind, but
        // some kernels honor `IPV6_V6ONLY=false` by leaving v4 addresses
        // for v4-only sockets. `[::]` on both sides is always a conflict.
        let held = TcpListener::bind(("::", 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();

        let cancel = CancellationToken::new();
        let res = spawn(port, cancel).await;
        let err = match res {
            Ok(_) => panic!("expected bind to fail"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        drop(held);
    }
}
