//! Integration test: full gRPC client ↔ server over localhost TCP.
//!
//! Spins up a real tonic server implementing `AgentApi`, then exercises the
//! production `GrpcServiceApi` + `AgentRuntime::bootstrap` path. This validates
//! the bearer interceptor, protobuf serialization, and bootstrap sequence
//! end-to-end.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};

use meshmon_protocol::{
    AgentApi, AgentApiServer, ConfigResponse, GetConfigRequest, GetTargetsRequest, MetricsBatch,
    PushMetricsResponse, PushRouteSnapshotResponse, RegisterRequest, RegisterResponse,
    RouteSnapshotRequest, Target, TargetsResponse,
};

use meshmon_agent::api::GrpcServiceApi;
use meshmon_agent::bootstrap::AgentRuntime;
use meshmon_agent::config::{AgentEnv, AgentIdentity};

// ---------------------------------------------------------------------------
// Mock server
// ---------------------------------------------------------------------------

/// Shared counters for the mock server, accessible from both the server
/// implementation and the test assertions.
struct MockCounters {
    register_count: AtomicUsize,
}

struct MockAgentApiServer {
    counters: Arc<MockCounters>,
}

impl MockAgentApiServer {
    fn new(counters: Arc<MockCounters>) -> Self {
        Self { counters }
    }
}

#[tonic::async_trait]
impl AgentApi for MockAgentApiServer {
    async fn register(
        &self,
        request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        self.counters.register_count.fetch_add(1, Ordering::SeqCst);
        let req = request.into_inner();
        if req.id.is_empty() {
            return Err(Status::invalid_argument("register id must not be empty"));
        }
        Ok(Response::new(RegisterResponse::default()))
    }

    async fn push_metrics(
        &self,
        _request: Request<MetricsBatch>,
    ) -> Result<Response<PushMetricsResponse>, Status> {
        Ok(Response::new(PushMetricsResponse::default()))
    }

    async fn push_route_snapshot(
        &self,
        _request: Request<RouteSnapshotRequest>,
    ) -> Result<Response<PushRouteSnapshotResponse>, Status> {
        Ok(Response::new(PushRouteSnapshotResponse::default()))
    }

    async fn get_config(
        &self,
        _request: Request<GetConfigRequest>,
    ) -> Result<Response<ConfigResponse>, Status> {
        Ok(Response::new(ConfigResponse::default()))
    }

    async fn get_targets(
        &self,
        _request: Request<GetTargetsRequest>,
    ) -> Result<Response<TargetsResponse>, Status> {
        let targets = vec![
            Target {
                id: "peer-a".to_string(),
                ip: vec![10, 0, 0, 1].into(),
                display_name: "Peer A".to_string(),
                location: "Test".to_string(),
                lat: 1.0,
                lon: 2.0,
            },
            Target {
                id: "peer-b".to_string(),
                ip: vec![10, 0, 0, 2].into(),
                display_name: "Peer B".to_string(),
                location: "Test".to_string(),
                lat: 3.0,
                lon: 4.0,
            },
        ];
        Ok(Response::new(TargetsResponse { targets }))
    }
}

// ---------------------------------------------------------------------------
// Helper: start mock server on a random port
// ---------------------------------------------------------------------------

async fn start_mock_server() -> (SocketAddr, Arc<MockCounters>) {
    let counters = Arc::new(MockCounters {
        register_count: AtomicUsize::new(0),
    });
    let mock = MockAgentApiServer::new(Arc::clone(&counters));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind");
    let addr = listener.local_addr().expect("failed to get local addr");

    // Use a oneshot channel to signal when the server task is scheduled and
    // about to start accepting. The kernel socket is already listening (bind
    // happened above), so receiving the signal guarantees the first accept()
    // will succeed immediately — no sleep needed.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        let _ = ready_tx.send(());
        tonic::transport::Server::builder()
            .add_service(AgentApiServer::new(mock))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("mock server error");
    });

    ready_rx
        .await
        .expect("server task dropped before signalling readiness");

    (addr, counters)
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bootstrap_against_real_grpc_server() {
    let (addr, counters) = start_mock_server().await;

    let env = AgentEnv {
        service_url: format!("http://{addr}"),
        agent_token: "test-token".to_string(),
        identity: AgentIdentity {
            id: "integration-agent".to_string(),
            display_name: "Integration Agent".to_string(),
            location: "Test".to_string(),
            ip: "127.0.0.1".parse().unwrap(),
            lat: 0.0,
            lon: 0.0,
        },
        agent_version: "0.0.0-test".to_string(),
    };

    let api = GrpcServiceApi::connect(&env.service_url, &env.agent_token)
        .await
        .expect("connect should succeed");

    let cancel = CancellationToken::new();
    let runtime = AgentRuntime::bootstrap(env, api, cancel.clone())
        .await
        .expect("bootstrap should succeed");

    assert_eq!(
        counters.register_count.load(Ordering::SeqCst),
        1,
        "register should have been called exactly once",
    );
    assert_eq!(
        runtime.supervisor_count(),
        2,
        "should have spawned two supervisors (peer-a and peer-b)",
    );

    cancel.cancel();
    runtime.shutdown().await;
}
