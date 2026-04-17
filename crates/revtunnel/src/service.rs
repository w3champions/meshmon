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
//! driver task calls `unregister(source_id, generation)` and the registry
//! entry goes away — but only if the stored generation still matches
//! this driver, so a reconnect's fresh entry is never clobbered by the
//! old driver's late exit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Callback invoked whenever the registered tunnel count changes. Hosted
/// binaries wire this to their typed `tunnel_agents` gauge accessor so this
/// crate does not need to reference any metric name literal directly —
/// preserving the "all `meshmon_*` literals live in the service crate's
/// metrics module" invariant.
pub type TunnelCountObserver = dyn Fn(usize) + Send + Sync + 'static;

/// Monotonic generation counter used to disambiguate tunnel entries when
/// the same `source_id` reconnects before the previous driver has finished
/// winding down. See `unregister` for the reason this exists.
static NEXT_GEN: AtomicU64 = AtomicU64::new(0);

/// Per-tunnel registration: the tonic Channel, the per-driver cancel token,
/// and a monotonic generation so reconnects don't clobber each other.
///
/// Storing the cancel token alongside the channel lets `close_all()` signal
/// every active driver to exit without a global master token that would
/// permanently prevent new drivers from running after a mid-lifecycle
/// `close_all()` call (e.g. the reconnect integration test).
struct TunnelEntry {
    channel: Channel,
    /// Cancel token that, when fired, causes the driver task to exit.
    driver_cancel: CancellationToken,
    /// Generation assigned at `accept()` time. The exiting driver task
    /// passes its own generation back into `unregister`; the map entry
    /// is removed only if generations match, so a reconnect that raced
    /// a previous driver's exit is never wiped out.
    generation: u64,
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
    /// Optional observer fired on every len change. Invoked under the
    /// registry mutex (short, synchronous closure) so callers must not
    /// re-enter the manager from inside the observer.
    observer: Option<Arc<TunnelCountObserver>>,
}

impl TunnelManager {
    /// Create an empty manager with no count observer.
    pub fn new() -> Self {
        Self {
            tunnels: Mutex::new(HashMap::new()),
            master_cancel: CancellationToken::new(),
            observer: None,
        }
    }

    /// Create an empty manager that invokes `observer(len)` whenever the
    /// registered tunnel count changes. Keep the closure cheap — it runs
    /// inside the registry mutex critical section.
    pub fn with_observer<F>(observer: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        Self {
            tunnels: Mutex::new(HashMap::new()),
            master_cancel: CancellationToken::new(),
            observer: Some(Arc::new(observer)),
        }
    }

    /// Accept an incoming `OpenTunnel` RPC. Returns the response body
    /// stream tonic will drive. Spawns a driver task that exits when any of:
    /// * this entry's `driver_cancel` is fired (`close_all()` for a live
    ///   entry, or replacement by a new `accept()` for the same `source_id`);
    /// * `master_cancel` is fired (hard shutdown);
    /// * the yamux session errors;
    /// * the remote end half-closes.
    ///
    /// # Mutex discipline
    ///
    /// The `tunnels` HashMap mutex is never held across `.await`. Every
    /// critical section is a synchronous HashMap operation only.
    pub async fn accept(
        self: Arc<Self>,
        source_id: String,
        incoming: tonic::Streaming<TunnelFrame>,
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
        // cascades automatically into this driver on hard shutdown.
        let effective = self.master_cancel.child_token();
        let generation = NEXT_GEN.fetch_add(1, Ordering::Relaxed);

        // Register. If a prior tunnel for this source_id existed, replace it
        // AND signal its driver to exit. Without the explicit cancel, the old
        // driver would linger until its own yamux session errored; its
        // eventual `unregister` call would then find a mismatched generation
        // and be a no-op, which is correct but wasteful. Cancelling the old
        // driver here makes the replacement prompt.
        let new_len = {
            let mut map = self.tunnels.lock().unwrap_or_else(|p| p.into_inner());
            let entry = TunnelEntry {
                channel,
                driver_cancel: effective.clone(),
                generation,
            };
            if let Some(old) = map.insert(source_id.clone(), entry) {
                debug!(source_id = %source_id, "replaced existing tunnel for source_id");
                // Cancel outside of any await (we're still synchronous here).
                old.driver_cancel.cancel();
            }
            let len = map.len();
            self.notify(len);
            len
        }; // mutex released here — no await below this point involves the lock
        let _ = new_len;

        // Driver: owns the yamux Connection and services:
        //   (a) outbound stream requests from the tonic connector, and
        //   (b) inbound yamux polling (required to drive the connection forward).
        //
        // The driver exits on effective-cancel, session error, or remote EOF,
        // then unregisters the source_id — but only if its generation matches.
        let manager = self.clone();
        let driver_source_id = source_id.clone();
        tokio::spawn(async move {
            drive_yamux_session(yamux_conn, stream_req_rx, effective, &driver_source_id).await;
            manager.unregister(&driver_source_id, generation);
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

    /// Remove a specific entry if — and only if — the stored generation
    /// matches the caller's. Driver tasks use this on session exit.
    ///
    /// # Why the generation guard matters
    ///
    /// Without it, the following race deletes a live tunnel:
    /// 1. Agent A connects, entry at generation 0 is inserted.
    /// 2. Network blip: A's tunnel starts to wind down; its driver is
    ///    still running but headed for its select-loop exit.
    /// 3. Agent A reconnects before the old driver exits. `accept` inserts
    ///    a fresh entry at generation 1, replacing the old one.
    /// 4. The old driver finally exits and calls `unregister("A")` — if
    ///    we unconditionally removed, the generation-1 entry is wiped
    ///    out and the fan-out layer can't reach agent A again until the
    ///    next reconnect.
    ///
    /// Comparing generations skips the stale removal. `accept` on the same
    /// `source_id` also proactively cancels the old `driver_cancel` so the
    /// displaced driver doesn't linger.
    fn unregister(&self, source_id: &str, generation: u64) {
        let len = {
            let mut map = self.tunnels.lock().unwrap_or_else(|p| p.into_inner());
            match map.get(source_id) {
                Some(entry) if entry.generation == generation => {
                    map.remove(source_id);
                    Some(map.len())
                }
                _ => None,
            }
        };
        if let Some(len) = len {
            self.notify(len);
        }
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
        let (tokens, new_len) = {
            let mut map = self.tunnels.lock().unwrap_or_else(|p| p.into_inner());
            let tokens: Vec<CancellationToken> =
                map.values().map(|e| e.driver_cancel.clone()).collect();
            map.clear();
            let len = map.len();
            self.notify(len);
            (tokens, len)
        };
        let _ = new_len;
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

    /// Fire the count observer with the latest registered len. Called from
    /// every mutating path so downstream metrics stay in lockstep with the
    /// map. Cheap no-op when no observer is wired.
    fn notify(&self, len: usize) {
        if let Some(obs) = self.observer.as_ref() {
            obs(len);
        }
    }
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Drive the yamux session.
///
/// Owns the `YamuxConnection` exclusively; no lock is needed. The loop is a
/// single `tokio::select!` so inbound frame processing, outbound substream
/// opening, new stream requests, and cancellation are all polled
/// concurrently. In particular, `poll_new_outbound` and `poll_next_inbound`
/// must race for each wakeup: outbound can only make progress once inbound
/// drains ACKs / window updates from the peer. Polling them sequentially
/// deadlocks on a full ACK backlog (outbound parks forever, inbound never
/// runs to drain it, cancel can't fire).
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
        // A single `poll_fn` that drives the yamux state machine forward:
        // both `poll_new_outbound` (when we have a pending request) and
        // `poll_next_inbound` (always, to flush ACKs / window updates)
        // share one `&mut conn` borrow. This is a Rust-borrow-checker
        // concession — two separate `poll_fn`s inside `select!` would each
        // try to re-borrow `conn` as mutable and won't compile. The logic
        // is equivalent: whichever of the two sub-polls makes progress
        // first short-circuits and returns, leaving the other to be tried
        // again on the next loop iteration.
        //
        // Returning `Poll::Pending` when neither side is ready ensures the
        // `select!` below correctly waits on the same waker for either
        // side to become ready.
        let yamux_step = poll_fn(|cx| -> std::task::Poll<YamuxStep> {
            // Try outbound first when we have a pending request. If
            // outbound is Pending, fall through to inbound so we keep
            // draining the session's ACK/window-update traffic —
            // otherwise outbound backpressure would deadlock.
            if pending_req.is_some() {
                if let std::task::Poll::Ready(out) = conn.poll_new_outbound(cx) {
                    return std::task::Poll::Ready(YamuxStep::Outbound(out));
                }
            }
            match conn.poll_next_inbound(cx) {
                std::task::Poll::Ready(next) => std::task::Poll::Ready(YamuxStep::Inbound(next)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        });

        tokio::select! {
            biased;

            _ = cancel.cancelled() => {
                debug!(source_id = %source_id, "tunnel driver: cancel fired");
                break;
            }

            // Only accept new stream requests when no outbound is in flight
            // — otherwise a second request would silently overwrite the first.
            req = stream_req_rx.recv(), if pending_req.is_none() => {
                match req {
                    Some(reply_tx) => pending_req = Some(reply_tx),
                    None => {
                        // Channel closed; connector is gone (channel dropped).
                        debug!(source_id = %source_id,
                               "yamux stream request channel closed");
                        break;
                    }
                }
            }

            step = yamux_step => match step {
                YamuxStep::Outbound(outbound) => {
                    // The outbound arm only fires when `pending_req.is_some()`;
                    // take it here to reply.
                    let reply_tx = pending_req.take()
                        .expect("outbound arm fires only while pending_req is Some");
                    match outbound {
                        Ok(stream) => {
                            // Ignore send error — connector may have timed out.
                            let _ = reply_tx.send(Ok(stream));
                        }
                        Err(e) => {
                            debug!(source_id = %source_id, error = %e,
                                   "yamux outbound failed; closing tunnel");
                            let _ = reply_tx.send(Err(e));
                            break;
                        }
                    }
                }
                YamuxStep::Inbound(Some(Ok(_inbound))) => {
                    warn!(source_id = %source_id,
                          "unexpected inbound substream on service-side tunnel; ignoring");
                }
                YamuxStep::Inbound(Some(Err(e))) => {
                    debug!(source_id = %source_id, error = %e,
                           "yamux session error; closing tunnel");
                    break;
                }
                YamuxStep::Inbound(None) => {
                    debug!(source_id = %source_id, "yamux session ended");
                    break;
                }
            }
        }
    }
}

/// Internal result of one yamux state-machine poll. Combining outbound +
/// inbound into a single enum lets us poll both from a single `&mut conn`
/// borrow inside one `poll_fn`, which is the only way tokio's `select!`
/// will accept two mutable uses of `conn` concurrently.
enum YamuxStep {
    Outbound(Result<yamux::Stream, yamux::ConnectionError>),
    Inbound(Option<Result<yamux::Stream, yamux::ConnectionError>>),
}

#[cfg(test)]
mod tests {
    use super::*;

    // TunnelManager::accept needs real yamux + tonic transport, so the
    // integration tests in Tasks 15-17 cover that path end-to-end.
    // Unit tests here focus on HashMap / observer / generation semantics
    // that don't need a live Channel.

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

    /// Regression for the unregister race described on `unregister` itself.
    ///
    /// Simulates a reconnect: the old driver's late exit must not delete
    /// the newer entry the reconnect just inserted.
    // Needs a tokio runtime because `Endpoint::connect_lazy` (used to fabricate
    // a `Channel` in `dummy_channel`) registers timers on the current reactor.
    #[tokio::test]
    async fn unregister_with_stale_generation_is_no_op() {
        let m = TunnelManager::new();

        // Stand in for what `accept()` would insert, minus the live channel:
        // we only need the generation + cancel token plumbing for the test,
        // and the map stores `TunnelEntry` which is module-private, so we
        // insert directly.
        //
        // Simulate two successive "accept"s for the same source_id —
        // generations 10 (old) and 11 (new). Then have the old driver
        // call `unregister` with generation 10.
        {
            let mut map = m.tunnels.lock().unwrap();
            map.insert(
                "agent-a".to_string(),
                TunnelEntry {
                    channel: dummy_channel(),
                    driver_cancel: CancellationToken::new(),
                    generation: 11,
                },
            );
        }

        m.unregister("agent-a", 10);

        assert_eq!(m.len(), 1, "stale unregister must not remove the new entry");
        let map = m.tunnels.lock().unwrap();
        let entry = map.get("agent-a").expect("entry still present");
        assert_eq!(entry.generation, 11);
    }

    /// Matching generation does remove.
    #[tokio::test]
    async fn unregister_with_matching_generation_removes() {
        let m = TunnelManager::new();
        {
            let mut map = m.tunnels.lock().unwrap();
            map.insert(
                "agent-b".to_string(),
                TunnelEntry {
                    channel: dummy_channel(),
                    driver_cancel: CancellationToken::new(),
                    generation: 7,
                },
            );
        }

        m.unregister("agent-b", 7);
        assert_eq!(m.len(), 0);
    }

    /// Observer sees the post-mutation len on each change.
    #[tokio::test]
    async fn observer_fires_on_mutations() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let last = Arc::new(AtomicUsize::new(usize::MAX));
        let sink = last.clone();
        let m = TunnelManager::with_observer(move |n| sink.store(n, Ordering::SeqCst));

        // Simulate a successful accept by inserting directly; then invoke
        // the private `notify` path by unregistering with a match.
        {
            let mut map = m.tunnels.lock().unwrap();
            map.insert(
                "agent-c".to_string(),
                TunnelEntry {
                    channel: dummy_channel(),
                    driver_cancel: CancellationToken::new(),
                    generation: 1,
                },
            );
        }

        // Observer hasn't been told yet — call notify directly to mirror what
        // accept does.
        m.notify(m.len());
        assert_eq!(last.load(Ordering::SeqCst), 1);

        m.unregister("agent-c", 1);
        assert_eq!(last.load(Ordering::SeqCst), 0);
    }

    /// A placeholder `Channel` for unit tests that never drive it. Built via
    /// `connect_lazy` against a dummy URI — no I/O occurs until an RPC is
    /// issued (which these tests never do).
    fn dummy_channel() -> Channel {
        Endpoint::try_from("http://unit-test.local")
            .expect("static URI parses")
            .connect_lazy()
    }
}
