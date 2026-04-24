//! TCP echo listener.
//!
//! Binds `[::]:port` dual-stack (`IPV6_V6ONLY=false`) and services TCP
//! connect-probes from peer agents over both IPv4 and IPv6. IPv4 peers
//! reach the listener transparently via IPv4-mapped IPv6 addresses
//! (`::ffff:a.b.c.d`). There is no application-level echo — peers measure
//! the TCP handshake itself. We accept the connection and close it
//! immediately; the kernel does a graceful FIN close, leaving the server
//! in a short TIME_WAIT. At the mesh's TCP probe rate this is negligible
//! (≈0.05 PPS per peer) and far preferable to an `SO_LINGER(0)` RST,
//! which races the client's non-blocking `connect()` completion on
//! same-host deployments and surfaces as spurious ECONNRESET.

use socket2::{Domain, Socket, Type};
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Backlog passed to `listen()` on the dual-stack socket. Matches the
/// value mio / rust-std pick by default on every Unix target — the
/// kernel clamps down to `SOMAXCONN` if smaller, so this is a safe
/// lower bound.
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
    // Match the stdlib/tokio default so a restarted agent can rebind the
    // port without waiting for TIME_WAIT to drain. mio's `TcpListener`
    // enables this implicitly; socket2 does not.
    socket.set_reuse_address(true)?;
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
                        // Dropping the stream triggers a graceful FIN close.
                        // Never force RST here — it races the client's
                        // non-blocking connect() on μs-RTT links.
                        drop(stream);
                        tracing::trace!(%peer, "tcp probe accepted");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tcp echo accept failed");
                        // Backoff a tiny amount to avoid hot-looping on
                        // something like EMFILE.
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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

        // Connect and expect the peer to close gracefully (FIN → EOF).
        // The listener never writes application data; the client sees zero
        // bytes on the first read. A previous SO_LINGER(0) variant sent RST
        // here, but that raced the client's non-blocking connect() on local
        // deployments and caused spurious ECONNRESET.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut buf = [0u8; 1];
        let n = tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf))
            .await
            .expect("read within 500 ms")
            .expect("clean EOF, not a socket error");
        assert_eq!(n, 0, "expected EOF, got {n} bytes");

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_connects_never_reset() {
        // Regression: the listener used to set SO_LINGER(0) and drop the
        // accepted stream, which RSTs the connection. On same-host
        // deployments (loopback, Docker bridge) the RST races the client's
        // non-blocking connect() completion — the `getsockopt(SO_ERROR)`
        // check after epoll-writable sees ECONNRESET and connect() returns
        // Err. The fix is to let the kernel do a graceful FIN close.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cancel = CancellationToken::new();
        let handle = spawn(port, cancel.clone()).await.expect("bind");

        const ATTEMPTS: usize = 50;
        let mut handles = Vec::with_capacity(ATTEMPTS);
        for _ in 0..ATTEMPTS {
            handles.push(tokio::spawn(async move {
                // connect() can already fail on Linux when the server's
                // RST races the final ACK. On macOS connect() succeeds
                // but a subsequent read() surfaces the reset. Both modes
                // must be caught; wrap both into a single Result.
                let mut s = TcpStream::connect(("127.0.0.1", port)).await?;
                let mut buf = [0u8; 1];
                let n = s.read(&mut buf).await?;
                Ok::<usize, std::io::Error>(n)
            }));
        }

        let mut failures: Vec<std::io::Error> = Vec::new();
        for h in handles {
            match h.await.unwrap() {
                Ok(n) => assert_eq!(n, 0, "listener must not write data"),
                Err(e) => failures.push(e),
            }
        }

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;

        assert!(
            failures.is_empty(),
            "{}/{} connects were reset; first error: {} (kind={:?})",
            failures.len(),
            ATTEMPTS,
            failures[0],
            failures[0].kind(),
        );
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
        // `[::]` vs `[::]` is always a conflict because both sockets
        // claim identical address families and kernel state regardless
        // of the `IPV6_V6ONLY` setting. A `0.0.0.0` squatter collides
        // on Linux and macOS today but relies on kernel-specific
        // v4/v6 cross-family behavior — `[::]` is the portable choice.
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
