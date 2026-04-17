//! Agent-side tunnel runner.
//!
//! `TunnelClient::open_and_run` opens the outer `OpenTunnel` RPC, wraps
//! the resulting bidi stream as `TunnelIo`, runs `yamux::Connection` in
//! server mode, and feeds each accepted yamux substream into a tonic
//! server that the caller built with its `AgentCommand` service registered.
//!
//! Returns `Ok(())` when the stream ends cleanly (service shutdown,
//! remote close). Returns `Err(TunnelError)` on any abnormal termination;
//! the caller's reconnect loop handles backoff.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::future::poll_fn;
use futures_util::StreamExt as _;
use meshmon_protocol::{AgentApiClient, TunnelFrame};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tonic::transport::server::Router;
use tracing::{debug, warn};
use yamux::{Config as YamuxConfig, Connection as YamuxConnection, Mode};

use crate::byte_adapter::TunnelIo;
use crate::error::TunnelError;

/// Opens the outer `OpenTunnel` RPC and runs yamux-over-gRPC on the
/// agent side.
pub struct TunnelClient;

impl TunnelClient {
    /// Open the outer bidi RPC, run yamux server mode on our half, and
    /// host the router's services on each substream yamux accepts.
    ///
    /// - `channel` is the raw tonic `Channel` (without an interceptor);
    ///   `OpenTunnel` multiplexes as another HTTP/2 stream on the same
    ///   connection shared with the unary RPCs.
    /// - `source_id` is stamped on `x-meshmon-source-id` metadata so the
    ///   service-side handler can validate and register this agent.
    /// - `agent_token` is the bearer token for this agent. Because `channel`
    ///   is a raw channel (no interceptor), the caller must pass the token
    ///   explicitly so this function can stamp `Authorization: Bearer <token>`
    ///   on the request metadata — matching the `BearerInterceptor` that gates
    ///   the other five RPCs on the service side.
    /// - `router_factory` returns a pre-configured `tonic::transport::server::Router`
    ///   (built via `Server::builder().add_service(AgentCommandServer::new(...))`).
    /// - `cancel` stops the loop on shutdown.
    pub async fn open_and_run(
        channel: Channel,
        source_id: &str,
        agent_token: &str,
        router_factory: impl FnOnce() -> Router + Send,
        cancel: CancellationToken,
    ) -> Result<(), TunnelError> {
        // 1. Outbound mpsc (agent → service frames). Capacity 16 caps
        //    worst-case buffering at ~1 MiB (16 × 64 KiB yamux frames).
        let (tx, rx) = mpsc::channel::<Result<TunnelFrame, tonic::Status>>(16);
        // Strip the Result wrapper — tonic client streams carry Ok items only.
        let body = ReceiverStream::new(rx)
            .filter_map(|r: Result<TunnelFrame, tonic::Status>| async move { r.ok() });
        let mut request = tonic::Request::new(body);
        request.metadata_mut().insert(
            "x-meshmon-source-id",
            source_id.parse().map_err(|_| {
                TunnelError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid source_id characters",
                ))
            })?,
        );
        // Stamp the bearer token on the request metadata. The raw `Channel`
        // does not carry an interceptor, so we inject the `Authorization`
        // header here directly — matching the `BearerInterceptor` the service
        // expects on every incoming RPC.
        let bearer_value = format!("Bearer {}", agent_token)
            .parse()
            .map_err(|_| {
                TunnelError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid agent_token characters",
                ))
            })?;
        request.metadata_mut().insert("authorization", bearer_value);

        // 2. Call OpenTunnel using the raw channel. Bearer auth is already
        //    stamped above on the request metadata.
        let mut client = AgentApiClient::new(channel);
        let response = client.open_tunnel(request).await?;
        let incoming = response.into_inner();

        // 3. Wrap (incoming, tx) as TunnelIo = AsyncRead + AsyncWrite, then
        //    convert from tokio::io traits to futures::io traits for yamux.
        let io = TunnelIo::new(incoming, tx).compat();

        // 4. Run yamux in server mode. Accept substreams on a background
        //    task and feed them into the tonic server via serve_with_incoming.
        let yamux_conn = YamuxConnection::new(io, YamuxConfig::default(), Mode::Server);
        let (substream_tx, substream_rx) =
            mpsc::unbounded_channel::<std::io::Result<SubstreamConnected>>();

        let yamux_task = tokio::spawn(drive_yamux(yamux_conn, substream_tx, cancel.clone()));

        // 5. serve_with_incoming consumes our substream channel.
        let router = router_factory();
        let incoming_substreams =
            tokio_stream::wrappers::UnboundedReceiverStream::new(substream_rx);

        let serve_result = tokio::select! {
            _ = cancel.cancelled() => {
                debug!("tunnel cancelled");
                Ok(())
            }
            r = router.serve_with_incoming(incoming_substreams) => r,
        };

        // Abort the yamux task; ignore any JoinError.
        yamux_task.abort();
        let _ = yamux_task.await;

        serve_result.map_err(TunnelError::from)
    }
}

async fn drive_yamux(
    mut yamux_conn: YamuxConnection<tokio_util::compat::Compat<TunnelIo>>,
    substream_tx: mpsc::UnboundedSender<std::io::Result<SubstreamConnected>>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("yamux driver: cancel fired");
                break;
            }
            next = poll_fn(|cx| yamux_conn.poll_next_inbound(cx)) => {
                match next {
                    Some(Ok(stream)) => {
                        let compat = stream.compat();
                        if substream_tx
                            .send(Ok(SubstreamConnected { inner: compat }))
                            .is_err()
                        {
                            // Server dropped the receiver; shut down the session.
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        warn!(error = %e, "yamux inbound error; closing tunnel");
                        break;
                    }
                    None => {
                        debug!("yamux session ended");
                        break;
                    }
                }
            }
        }
    }
}

/// Adapter around a yamux substream that satisfies
/// `tonic::transport::server::Connected` + `AsyncRead + AsyncWrite`.
pub(crate) struct SubstreamConnected {
    inner: tokio_util::compat::Compat<yamux::Stream>,
}

impl tonic::transport::server::Connected for SubstreamConnected {
    type ConnectInfo = ();
    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl AsyncRead for SubstreamConnected {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        Pin::new(&mut me.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for SubstreamConnected {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let me = self.get_mut();
        Pin::new(&mut me.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        Pin::new(&mut me.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        Pin::new(&mut me.inner).poll_shutdown(cx)
    }
}
