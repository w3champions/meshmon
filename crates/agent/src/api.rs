//! gRPC API client for communicating with the meshmon service.
//!
//! [`ServiceApi`] abstracts the five RPCs the agent uses so that production
//! code goes through [`GrpcServiceApi`] while tests can substitute a mock.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tonic::service::interceptor::InterceptedService;
use tonic::service::Interceptor;
use tonic::transport::{Channel, ClientTlsConfig};

use meshmon_protocol::{
    AgentApiClient, ConfigResponse, GetConfigRequest, GetTargetsRequest, MetricsBatch,
    PushMetricsResponse, PushRouteSnapshotResponse, RegisterRequest, RegisterResponse,
    RouteSnapshotRequest, TargetsResponse,
};

#[allow(dead_code)]
fn _assert_client_clone_send_sync() {
    fn is_clone_send_sync<T: Clone + Send + Sync + 'static>() {}
    is_clone_send_sync::<AgentApiClient<InterceptedService<Channel, BearerInterceptor>>>();
}

// ---------------------------------------------------------------------------
// ServiceApi trait
// ---------------------------------------------------------------------------

/// Abstraction over the five agent-facing RPCs.
///
/// Implemented by [`GrpcServiceApi`] for production and by test doubles in
/// integration tests. The trait is `Send + Sync + 'static` so it can live
/// behind `Arc<dyn ServiceApi>`.
#[allow(async_fn_in_trait)]
pub trait ServiceApi: Send + Sync + 'static {
    /// Register this agent with the service (or refresh its metadata).
    async fn register(&self, req: RegisterRequest) -> Result<RegisterResponse>;

    /// Fetch the current probe configuration from the service.
    async fn get_config(&self) -> Result<ConfigResponse>;

    /// Fetch the list of active probe targets, excluding `source_id` itself.
    async fn get_targets(&self, source_id: &str) -> Result<TargetsResponse>;

    /// Push a batch of aggregated probe metrics.
    async fn push_metrics(&self, batch: MetricsBatch) -> Result<PushMetricsResponse>;

    /// Push a route-change snapshot.
    async fn push_route_snapshot(
        &self,
        req: RouteSnapshotRequest,
    ) -> Result<PushRouteSnapshotResponse>;
}

// ---------------------------------------------------------------------------
// Bearer interceptor
// ---------------------------------------------------------------------------

/// Tonic interceptor that attaches `Authorization: Bearer <token>` to every
/// outgoing request.
#[derive(Clone)]
struct BearerInterceptor {
    token: Arc<str>,
}

impl Interceptor for BearerInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> std::result::Result<tonic::Request<()>, tonic::Status> {
        let value = format!("Bearer {}", self.token)
            .parse()
            .map_err(|_| tonic::Status::internal("invalid bearer token"))?;
        request.metadata_mut().insert("authorization", value);
        Ok(request)
    }
}

// ---------------------------------------------------------------------------
// GrpcServiceApi
// ---------------------------------------------------------------------------

/// Production [`ServiceApi`] backed by a tonic gRPC channel.
///
/// The intercepted client is `Clone` (cheap — clones share the underlying
/// HTTP/2 connection via tonic's `Channel` Arc semantics), so concurrent
/// RPCs from different tasks multiplex over one connection instead of
/// serializing through a mutex.
pub struct GrpcServiceApi {
    client: AgentApiClient<InterceptedService<Channel, BearerInterceptor>>,
}

impl GrpcServiceApi {
    /// Create a new client connected (lazily) to `service_url`.
    ///
    /// The channel is created with `connect_lazy()` so construction never
    /// blocks. HTTP/2 keep-alive pings are sent every 60 seconds with a
    /// 20-second response deadline so the connection stays warm across long
    /// probe intervals and dead peers are detected promptly.
    ///
    /// When `service_url` uses the `https://` scheme, TLS is configured with
    /// the OS certificate store (`tls-native-roots` feature). Plain `http://`
    /// URLs use cleartext h2c — the service is expected to be inside a
    /// trusted network in that case.
    pub async fn connect(service_url: &str, agent_token: &str) -> Result<Arc<Self>> {
        let mut endpoint = Channel::from_shared(service_url.to_owned())
            .context("invalid service URL")?
            .keep_alive_timeout(Duration::from_secs(20))
            .http2_keep_alive_interval(Duration::from_secs(60))
            .keep_alive_while_idle(true);

        // Channel::from_shared does not auto-apply TLS for https:// URLs;
        // configure it explicitly so HTTPS endpoints actually negotiate TLS.
        if service_url.to_ascii_lowercase().starts_with("https://") {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_enabled_roots())
                .context("failed to configure TLS")?;
        }

        let channel = endpoint.connect_lazy();

        let interceptor = BearerInterceptor {
            token: Arc::from(agent_token),
        };

        let client = AgentApiClient::with_interceptor(channel, interceptor);

        Ok(Arc::new(Self { client }))
    }
}

impl ServiceApi for GrpcServiceApi {
    async fn register(&self, req: RegisterRequest) -> Result<RegisterResponse> {
        self.client
            .clone()
            .register(req)
            .await
            .map(|r| r.into_inner())
            .context("Register RPC failed")
    }

    async fn get_config(&self) -> Result<ConfigResponse> {
        self.client
            .clone()
            .get_config(GetConfigRequest {})
            .await
            .map(|r| r.into_inner())
            .context("GetConfig RPC failed")
    }

    async fn get_targets(&self, source_id: &str) -> Result<TargetsResponse> {
        self.client
            .clone()
            .get_targets(GetTargetsRequest {
                source_id: source_id.to_owned(),
            })
            .await
            .map(|r| r.into_inner())
            .context("GetTargets RPC failed")
    }

    async fn push_metrics(&self, batch: MetricsBatch) -> Result<PushMetricsResponse> {
        self.client
            .clone()
            .push_metrics(batch)
            .await
            .map(|r| r.into_inner())
            .context("PushMetrics RPC failed")
    }

    async fn push_route_snapshot(
        &self,
        req: RouteSnapshotRequest,
    ) -> Result<PushRouteSnapshotResponse> {
        self.client
            .clone()
            .push_route_snapshot(req)
            .await
            .map(|r| r.into_inner())
            .context("PushRouteSnapshot RPC failed")
    }
}
