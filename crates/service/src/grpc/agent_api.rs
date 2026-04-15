//! Tonic service implementation for the five agent RPCs.

use crate::state::AppState;
use meshmon_protocol::{
    ip as proto_ip, AgentApi, ConfigResponse, GetConfigRequest, GetTargetsRequest, MetricsBatch,
    PushMetricsResponse, PushRouteSnapshotResponse, RegisterRequest, RegisterResponse,
    RouteSnapshotRequest, TargetsResponse,
};
use tonic::{Request, Response, Status};

/// Concrete implementation of the `AgentApi` tonic service.
///
/// Each RPC body is a stub returning [`Status::unimplemented`]; the real
/// implementations arrive in Tasks 10–12, 15–16.
pub struct AgentApiImpl {
    state: AppState,
}

impl AgentApiImpl {
    /// Construct a new impl from the shared application state.
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Borrow the shared application state.
    pub fn state(&self) -> &AppState {
        &self.state
    }
}

#[tonic::async_trait]
impl AgentApi for AgentApiImpl {
    #[tracing::instrument(skip_all, fields(agent_id = tracing::field::Empty))]
    async fn register(
        &self,
        request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        // Step 1: connection IP (preserved by grpc_harness::StreamWithPeer
        // in tests; preserved by main.rs `auto::Builder` loop in production).
        //
        // `tonic::Request::remote_addr()` only knows `TcpConnectInfo`. The
        // in-process test harness inserts a bare `SocketAddr` directly (via
        // the `Connected` impl on `StreamWithPeer`), and production inserts
        // `axum::extract::ConnectInfo<SocketAddr>`. We try all three.
        let trust_forwarded = self.state.config().service.trust_forwarded_headers;
        let extract_peer_addr = |req: &Request<RegisterRequest>| {
            // 1. tonic native (TcpConnectInfo — real TcpListener)
            req.remote_addr()
                // 2. bare SocketAddr (in-process StreamWithPeer harness)
                .or_else(|| req.extensions().get::<std::net::SocketAddr>().copied())
                // 3. axum ConnectInfo (production hyper integration path)
                .or_else(|| {
                    req.extensions()
                        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                        .map(|ci| ci.0)
                })
                .map(|a| a.ip())
        };
        let peer_ip = if trust_forwarded {
            // Check metadata first (proxies inject XFF); fall back to remote_addr.
            request
                .metadata()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.split(',').next())
                .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
                .or_else(|| extract_peer_addr(&request))
        } else {
            extract_peer_addr(&request)
        }
        .ok_or_else(|| Status::invalid_argument("no client IP on request"))?;

        let req = request.into_inner();
        tracing::Span::current().record("agent_id", tracing::field::display(&req.id));

        // Step 2: payload validation.
        if req.id.trim().is_empty() || req.display_name.trim().is_empty() {
            return Err(Status::invalid_argument("id and display_name required"));
        }
        let claimed_ip = proto_ip::to_ipaddr(&req.ip)
            .map_err(|e| Status::invalid_argument(format!("invalid ip bytes: {e}")))?;

        // Step 3: claimed IP must match connection IP (loopback exempt).
        if !peer_ip.is_loopback() && peer_ip != claimed_ip {
            tracing::warn!(
                %peer_ip, %claimed_ip, agent_id = %req.id,
                "register: claimed IP does not match connection IP",
            );
            return Err(Status::permission_denied(
                "claimed IP does not match connection IP",
            ));
        }

        // Step 4: same-ID different-IP preflight.
        let claimed_ip_net = sqlx::types::ipnetwork::IpNetwork::from(claimed_ip);
        let existing = sqlx::query!(
            r#"SELECT ip as "ip: sqlx::types::ipnetwork::IpNetwork"
               FROM agents WHERE id = $1"#,
            req.id
        )
        .fetch_optional(&self.state.pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "register: preflight select failed");
            Status::internal("register preflight failed")
        })?;
        if let Some(row) = existing {
            if row.ip.ip() != claimed_ip {
                tracing::warn!(
                    agent_id = %req.id,
                    existing_ip = %row.ip.ip(),
                    claimed_ip = %claimed_ip,
                    "register: id already registered with a different IP",
                );
                return Err(Status::already_exists(
                    "id already registered with a different IP",
                ));
            }
        }

        // Step 5: upsert (ip NOT in SET list).
        let location = (!req.location.is_empty()).then(|| req.location.clone());
        let agent_version = (!req.agent_version.is_empty()).then(|| req.agent_version.clone());
        sqlx::query!(
            r#"INSERT INTO agents
                  (id, display_name, location, ip, lat, lon, agent_version,
                   registered_at, last_seen_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())
               ON CONFLICT (id) DO UPDATE SET
                   display_name  = EXCLUDED.display_name,
                   location      = EXCLUDED.location,
                   lat           = EXCLUDED.lat,
                   lon           = EXCLUDED.lon,
                   agent_version = EXCLUDED.agent_version,
                   last_seen_at  = NOW()"#,
            req.id,
            req.display_name,
            location,
            claimed_ip_net,
            req.lat,
            req.lon,
            agent_version,
        )
        .execute(&self.state.pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "register upsert failed");
            Status::internal("register upsert failed")
        })?;

        // Step 6: synchronous registry refresh so the next push sees it.
        if let Err(e) = self.state.registry.force_refresh().await {
            tracing::warn!(
                error = %e,
                "register: registry force_refresh failed; periodic refresh will catch up",
            );
        }

        Ok(Response::new(RegisterResponse::default()))
    }

    async fn push_metrics(
        &self,
        _request: Request<MetricsBatch>,
    ) -> Result<Response<PushMetricsResponse>, Status> {
        Err(Status::unimplemented("push_metrics"))
    }

    async fn push_route_snapshot(
        &self,
        _request: Request<RouteSnapshotRequest>,
    ) -> Result<Response<PushRouteSnapshotResponse>, Status> {
        Err(Status::unimplemented("push_route_snapshot"))
    }

    async fn get_config(
        &self,
        _request: Request<GetConfigRequest>,
    ) -> Result<Response<ConfigResponse>, Status> {
        Err(Status::unimplemented("get_config"))
    }

    async fn get_targets(
        &self,
        _request: Request<GetTargetsRequest>,
    ) -> Result<Response<TargetsResponse>, Status> {
        Err(Status::unimplemented("get_targets"))
    }
}
