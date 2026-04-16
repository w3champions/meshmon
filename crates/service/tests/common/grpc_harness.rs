//! In-process tonic server + generated client for integration tests.

use super::TEST_AGENT_TOKEN;
use meshmon_protocol::{AgentApiClient, AgentApiServer};
use meshmon_service::grpc::{agent_api::AgentApiImpl, MAX_GRPC_DECODING_BYTES};
use meshmon_service::http::auth::{agent_grpc_interceptor, PeerAddrKeyExtractor};
use meshmon_service::state::AppState;
use std::net::{IpAddr, SocketAddr};
use tokio::io::DuplexStream;
use tonic::metadata::MetadataValue;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint, Server, Uri};
use tower::service_fn;
use tower_governor::GovernorLayer;

struct StreamWithPeer {
    inner: DuplexStream,
    peer: SocketAddr,
}

impl tonic::transport::server::Connected for StreamWithPeer {
    type ConnectInfo = SocketAddr;
    fn connect_info(&self) -> Self::ConnectInfo {
        self.peer
    }
}

impl tokio::io::AsyncRead for StreamWithPeer {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let me = self.get_mut();
        std::pin::Pin::new(&mut me.inner).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for StreamWithPeer {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        let me = self.get_mut();
        std::pin::Pin::new(&mut me.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        let me = self.get_mut();
        std::pin::Pin::new(&mut me.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        let me = self.get_mut();
        std::pin::Pin::new(&mut me.inner).poll_shutdown(cx)
    }
}

pub type TestClient = AgentApiClient<
    InterceptedService<
        Channel,
        Box<dyn Fn(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> + Send + Sync>,
    >,
>;

/// Build a rate-limit layer typed for `tonic::body::Body` (the response body
/// produced by tonic's `Routes`). The production helper is parameterized with
/// `axum::body::Body`, which is incompatible with tonic's `Server::builder().layer()`.
/// We duplicate the config logic here so the harness can exercise the same
/// per-IP rate limit without changing the production API.
fn tonic_rate_limit_layer(
    trust_forwarded: bool,
    per_minute: u32,
    burst: u32,
) -> tower::util::Either<
    GovernorLayer<
        tower_governor::key_extractor::SmartIpKeyExtractor,
        governor::middleware::NoOpMiddleware,
        tonic::body::Body,
    >,
    GovernorLayer<PeerAddrKeyExtractor, governor::middleware::NoOpMiddleware, tonic::body::Body>,
> {
    use tower_governor::governor::GovernorConfigBuilder;
    use tower_governor::key_extractor::SmartIpKeyExtractor;

    // Mirror production: nanosecond precision so `per_minute` is honored
    // across non-multiples-of-60 (see meshmon_service::http::auth).
    let period_nanos = 60_000_000_000u64
        .checked_div(u64::from(per_minute.max(1)))
        .unwrap_or(1_000_000_000)
        .max(1);

    if trust_forwarded {
        let cfg = GovernorConfigBuilder::default()
            .per_nanosecond(period_nanos)
            .burst_size(burst)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("governor config (smart)");
        tower::util::Either::Left(GovernorLayer::new(cfg))
    } else {
        let cfg = GovernorConfigBuilder::default()
            .per_nanosecond(period_nanos)
            .burst_size(burst)
            .key_extractor(PeerAddrKeyExtractor)
            .finish()
            .expect("governor config (peer-only)");
        tower::util::Either::Right(GovernorLayer::new(cfg))
    }
}

pub async fn in_process_agent_client(state: AppState, peer_ip: IpAddr) -> TestClient {
    in_process_agent_client_with_token(state, peer_ip, TEST_AGENT_TOKEN).await
}

#[allow(clippy::type_complexity)]
pub async fn in_process_agent_client_with_token(
    state: AppState,
    peer_ip: IpAddr,
    token: &str,
) -> TestClient {
    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let peer = SocketAddr::new(peer_ip, 40_000);

    let cfg = state.config();
    // Build the rate-limit layer typed with tonic::body::Body so it's
    // compatible with Server::builder().layer(); the production helper uses
    // axum::body::Body, which is incompatible here (see tonic_rate_limit_layer
    // comment above).
    let rate_limit = tonic_rate_limit_layer(
        cfg.service.trust_forwarded_headers,
        cfg.agent_api.rate_limit_per_minute,
        cfg.agent_api.rate_limit_burst,
    );
    let impl_ = AgentApiImpl::new(state.clone());
    // Build the sized server first; `InterceptedService` does not forward
    // `max_decoding_message_size`, so we must set it on `AgentApiServer`
    // before wrapping — matching the pattern established in grpc/mod.rs.
    let sized_server =
        AgentApiServer::new(impl_).max_decoding_message_size(MAX_GRPC_DECODING_BYTES);
    let svc = tonic::service::interceptor::InterceptedService::new(
        sized_server,
        agent_grpc_interceptor(state.clone()),
    );

    tokio::spawn(async move {
        let _ = Server::builder()
            .layer(rate_limit)
            .add_service(svc)
            .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(
                StreamWithPeer {
                    inner: server_stream,
                    peer,
                },
            )))
            .await;
    });

    let mut client_stream = Some(client_stream);
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(service_fn(move |_: Uri| {
            let stream = client_stream.take().expect("connector called twice");
            async move { Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream)) }
        }))
        .await
        .expect("in-process channel connect");

    let bearer: MetadataValue<_> = format!("Bearer {token}").parse().unwrap();
    let interceptor: Box<
        dyn Fn(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> + Send + Sync,
    > = Box::new(move |mut req: tonic::Request<()>| {
        req.metadata_mut().insert("authorization", bearer.clone());
        Ok(req)
    });
    AgentApiClient::with_interceptor(channel, interceptor)
}
