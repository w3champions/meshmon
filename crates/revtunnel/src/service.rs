//! Service-side tunnel manager.
//!
//! Holds one `tonic::transport::Channel` per connected agent, keyed by
//! `source_id`. A Channel is cheap to clone (Arc semantics) so callers
//! can snapshot + fan out concurrent RPCs.
//!
//! When the `OpenTunnel` RPC handler fires on the service, it calls
//! `TunnelManager::accept(...)`. `accept` builds a `TunnelIo` from the
//! inbound tonic stream, runs yamux in client mode, constructs a
//! `Channel` that opens a yamux substream per logical RPC, and stores
//! `(source_id, channel)` in the registry. The returned `ReceiverStream`
//! is what the RPC handler yields as its response body.
//!
//! On stream termination (client disconnect, service shutdown), the
//! driver task calls `unregister(source_id)` and the registry entry
//! goes away.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::future::poll_fn;
use meshmon_protocol::TunnelFrame;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, Endpoint};
use tonic::Status;
use tracing::{debug, warn};
use yamux::{Config as YamuxConfig, Connection as YamuxConnection, Mode};

use crate::byte_adapter::TunnelIo;
use crate::error::TunnelError;

/// A pending request to open a new yamux outbound substream.
/// The driver task fulfills these during its poll cycle.
type StreamRequest = oneshot::Sender<Result<yamux::Stream, yamux::ConnectionError>>;

/// Per-tunnel registration: the tonic Channel plus the per-driver cancel token.
///
/// Storing the cancel token alongside the channel lets `close_all()` signal
/// every active driver to exit without a global master token that would
/// permanently prevent new drivers from running after a mid-lifecycle
/// `close_all()` call (e.g. the reconnect integration test).
struct TunnelEntry {
    channel: Channel,
    /// Cancel token that, when fired, causes the driver task to exit.
    driver_cancel: CancellationToken,
}

/// Service-side registry of per-agent reverse-tunnel Channels.
pub struct TunnelManager {
    tunnels: Mutex<HashMap<String, TunnelEntry>>,
    /// Master cancellation token: parent of every per-driver child token.
    ///
    /// Each driver's effective cancel token is created as
    /// `master_cancel.child_token()`, meaning cancelling `master_cancel`
    /// automatically cascades into every currently-running driver — a useful
    /// escape hatch for hard shutdown scenarios.
    ///
    /// `close_all()` does NOT cancel this token; it cancels each stored
    /// `driver_cancel` individually. This means the manager remains usable
    /// for new `accept()` calls after `close_all()` (e.g. reconnect tests).
    master_cancel: CancellationToken,
}

impl TunnelManager {
    /// Create an empty manager.
    pub fn new() -> Self {
        Self {
            tunnels: Mutex::new(HashMap::new()),
            master_cancel: CancellationToken::new(),
        }
    }

    /// Accept an incoming `OpenTunnel` RPC. Returns the response body
    /// stream tonic will drive. Spawns a driver task that exits when either
    /// the caller's `cancel` fires, `close_all()` is called (master cancel),
    /// the session errors, or the remote end half-closes.
    ///
    /// # Mutex discipline
    ///
    /// The `tunnels` HashMap mutex is never held across `.await`. Every
    /// critical section is a synchronous HashMap operation only.
    pub async fn accept(
        self: Arc<Self>,
        source_id: String,
        incoming: tonic::Streaming<TunnelFrame>,
        cancel: CancellationToken,
    ) -> Result<ReceiverStream<Result<TunnelFrame, Status>>, TunnelError> {
        let (out_tx, out_rx) = mpsc::channel::<Result<TunnelFrame, Status>>(16);

        // yamux needs futures_io::AsyncRead/Write; TunnelIo is tokio-io.
        // TokioAsyncReadCompatExt::compat() bridges the two trait families.
        let io = TunnelIo::new(incoming, out_tx).compat();
        let yamux_conn = YamuxConnection::new(io, YamuxConfig::default(), Mode::Client);

        // Channel for the tonic connector to request new yamux outbound substreams.
        // The driver task owns the Connection and fulfills requests here.
        let (stream_req_tx, stream_req_rx) = mpsc::channel::<StreamRequest>(8);

        // Build the tonic Channel via a custom connector. Each logical RPC
        // triggers the connector, which sends a oneshot over `stream_req_tx`
        // and waits for the driver to open a yamux substream and reply.
        let connector = {
            let stream_req_tx = stream_req_tx.clone();
            tower::service_fn(move |_uri: tonic::transport::Uri| {
                let stream_req_tx = stream_req_tx.clone();
                async move {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    stream_req_tx.send(reply_tx).await.map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "yamux driver has exited",
                        )
                    })?;
                    let stream = reply_rx
                        .await
                        .map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::BrokenPipe,
                                "yamux driver dropped stream request",
                            )
                        })?
                        .map_err(|e| {
                            std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())
                        })?;
                    // yamux::Stream implements futures_io; bridge back to tokio-io
                    // with FuturesAsyncReadCompatExt, then wrap in TokioIo for hyper.
                    let tokio_io = stream.compat();
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(tokio_io))
                }
            })
        };

        let endpoint = Endpoint::try_from("http://tunnel.local")
            .expect("static URI always parses")
            .connect_timeout(std::time::Duration::from_secs(5));

        // connect_with_connector_lazy avoids blocking accept() on a real TCP
        // handshake; the first RPC call will trigger the connector.
        let channel = endpoint.connect_with_connector_lazy(connector);

        // Effective cancel: a child of master_cancel so master's cancellation
        // cascades automatically. A bridge task additionally cancels it when
        // the caller's cancel fires, so either side can tear the driver down.
        let effective = self.master_cancel.child_token();
        let bridge = {
            let effective = effective.clone();
            tokio::spawn(async move {
                cancel.cancelled().await;
                effective.cancel();
            })
        };

        // Register. If a prior tunnel for this source_id existed, replace it
        // (the old driver_cancel is dropped; the old driver will naturally
        // detect its yamux session is gone). In-flight RPCs on the old Channel
        // will observe UNAVAILABLE. Callers handle idempotently.
        {
            let mut map = self.tunnels.lock().unwrap_or_else(|p| p.into_inner());
            let entry = TunnelEntry {
                channel,
                driver_cancel: effective.clone(),
            };
            if map.insert(source_id.clone(), entry).is_some() {
                debug!(source_id = %source_id, "replaced existing tunnel for source_id");
            }
            update_gauge(map.len());
        } // mutex released here — no await below this point involves the lock

        // Driver: owns the yamux Connection and services:
        //   (a) outbound stream requests from the tonic connector, and
        //   (b) inbound yamux polling (required to drive the connection forward).
        //
        // The driver exits on effective-cancel, session error, or remote EOF,
        // then aborts the bridge task and unregisters the source_id.
        let manager = self.clone();
        let driver_source_id = source_id.clone();
        tokio::spawn(async move {
            drive_yamux_session(yamux_conn, stream_req_rx, effective, &driver_source_id).await;
            bridge.abort();
            manager.unregister(&driver_source_id);
        });

        Ok(ReceiverStream::new(out_rx))
    }

    /// Returns a cheap clone of the registered Channel, if any.
    pub fn channel_for(&self, source_id: &str) -> Option<Channel> {
        self.tunnels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .map(|e| e.channel.clone())
    }

    /// Snapshot the full registry. Callers iterate outside the lock.
    pub fn snapshot(&self) -> Vec<(String, Channel)> {
        self.tunnels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(k, e)| (k.clone(), e.channel.clone()))
            .collect()
    }

    /// Remove a specific entry (driver task uses this on session exit).
    fn unregister(&self, source_id: &str) {
        let mut map = self.tunnels.lock().unwrap_or_else(|p| p.into_inner());
        map.remove(source_id);
        update_gauge(map.len());
    }

    /// Drop every registered Channel and cancel all active driver tasks.
    ///
    /// Cancels each registered driver's individual cancel token, causing every
    /// active driver to exit its select loop, drop the yamux connection, and let
    /// the outbound mpsc sender go out of scope. This makes the `ReceiverStream`
    /// returned from `accept` EOF, which in turn lets tonic's response body
    /// complete and the server's graceful-shutdown drain proceed.
    ///
    /// After `close_all()` the manager is empty. New calls to `accept()` will
    /// work normally (they receive fresh child tokens from `master_cancel`).
    ///
    /// For a hard terminal shutdown — where no new tunnels should be accepted
    /// after this point — cancel the `master_cancel` token externally (or use
    /// the service-level shutdown token to stop accepting new connections).
    /// In practice, production shutdown already stops the gRPC listener before
    /// draining, so no new `accept()` calls arrive after `close_all()`.
    pub fn close_all(&self) {
        // Collect and cancel all driver tokens under the lock, then clear.
        let tokens: Vec<CancellationToken> = {
            let mut map = self.tunnels.lock().unwrap_or_else(|p| p.into_inner());
            let tokens = map.values().map(|e| e.driver_cancel.clone()).collect();
            map.clear();
            update_gauge(0);
            tokens
        };
        // Cancel after releasing the lock so drivers can unregister without
        // deadlocking on the mutex.
        for token in tokens {
            token.cancel();
        }
    }

    /// Current registered count (for tests / metrics parity checks).
    pub fn len(&self) -> usize {
        self.tunnels.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Convenience for `len() == 0`.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Drive the yamux session.
///
/// Owns the `YamuxConnection` exclusively; no lock is needed. The loop:
/// - Polls `poll_next_inbound` to drive the connection state machine forward
///   (required even on the client side — yamux needs it to process ACKs,
///   window updates, and to flush outbound buffers).
/// - Services pending `StreamRequest`s: for each pending oneshot, calls
///   `poll_new_outbound` and replies when a stream is available.
///
/// Service-side is yamux **Client** mode. Inbound substreams from the agent
/// are unexpected by contract (agents only accept, they don't initiate), so
/// unexpected inbound streams are logged and ignored.
async fn drive_yamux_session<T>(
    mut conn: YamuxConnection<T>,
    mut stream_req_rx: mpsc::Receiver<StreamRequest>,
    cancel: CancellationToken,
    source_id: &str,
) where
    T: futures_util::AsyncRead + futures_util::AsyncWrite + Unpin,
{
    // Pending outbound request, if one is in-flight.
    let mut pending_req: Option<StreamRequest> = None;

    loop {
        // If we have a pending outbound request, try to fulfill it first.
        if let Some(reply_tx) = pending_req.take() {
            // poll_new_outbound may return Pending if the backlog is full;
            // if so we stash the request back and let poll_next_inbound make
            // progress to drain the backlog.
            match poll_fn(|cx| conn.poll_new_outbound(cx)).await {
                Ok(stream) => {
                    // Ignore send error — connector may have timed out.
                    let _ = reply_tx.send(Ok(stream));
                }
                Err(e) => {
                    debug!(source_id = %source_id, error = %e, "yamux outbound failed; closing tunnel");
                    let _ = reply_tx.send(Err(e));
                    break;
                }
            }
            continue;
        }

        // No pending outbound request — wait for either cancellation,
        // a new stream request, or an inbound event (drives the connection).
        tokio::select! {
            biased;

            _ = cancel.cancelled() => {
                debug!(source_id = %source_id, "tunnel driver: cancel fired");
                break;
            }

            req = stream_req_rx.recv() => {
                match req {
                    Some(reply_tx) => {
                        // Queue it; will be fulfilled at top of next loop iteration.
                        pending_req = Some(reply_tx);
                    }
                    None => {
                        // Channel closed; connector is gone (channel dropped).
                        debug!(source_id = %source_id, "yamux stream request channel closed");
                        break;
                    }
                }
            }

            next = poll_fn(|cx| conn.poll_next_inbound(cx)) => {
                match next {
                    Some(Ok(_inbound)) => {
                        // Service is yamux client — agent-initiated substreams
                        // are not part of the current contract. Log and drop.
                        warn!(source_id = %source_id,
                              "unexpected inbound substream on service-side tunnel; ignoring");
                    }
                    Some(Err(e)) => {
                        debug!(source_id = %source_id, error = %e,
                               "yamux session error; closing tunnel");
                        break;
                    }
                    None => {
                        debug!(source_id = %source_id, "yamux session ended");
                        break;
                    }
                }
            }
        }
    }
}

fn update_gauge(len: usize) {
    // The service crate's metrics module owns the descriptor; this call
    // is a no-op before that module has registered the name.
    metrics::gauge!("meshmon_service_tunnel_agents").set(len as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    // TunnelManager::accept needs real yamux + tonic transport, so the
    // integration tests in Tasks 15-17 cover that path end-to-end.
    // Unit tests here focus on HashMap / gauge-parity semantics that
    // don't need a live Channel.

    #[test]
    fn new_manager_is_empty() {
        let m = TunnelManager::new();
        assert_eq!(m.len(), 0);
        assert!(m.is_empty());
    }

    #[test]
    fn close_all_is_idempotent_on_empty() {
        let m = TunnelManager::new();
        m.close_all();
        m.close_all();
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn snapshot_on_empty_is_empty() {
        let m = TunnelManager::new();
        assert!(m.snapshot().is_empty());
    }

    #[test]
    fn channel_for_missing_is_none() {
        let m = TunnelManager::new();
        assert!(m.channel_for("nobody").is_none());
    }
}
