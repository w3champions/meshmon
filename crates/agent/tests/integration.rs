//! Integration test: full gRPC client ↔ server over localhost TCP.
//!
//! Spins up a real tonic server implementing `AgentApi`, then exercises the
//! production `GrpcServiceApi` + `AgentRuntime::bootstrap` path. This validates
//! the bearer interceptor, protobuf serialization, and bootstrap sequence
//! end-to-end.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use tokio::net::{TcpListener, UdpSocket};
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
    push_metrics_batches: StdMutex<Vec<MetricsBatch>>,
    push_route_snapshots: StdMutex<Vec<RouteSnapshotRequest>>,
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
        request: Request<MetricsBatch>,
    ) -> Result<Response<PushMetricsResponse>, Status> {
        self.counters
            .push_metrics_batches
            .lock()
            .expect("poisoned")
            .push(request.into_inner());
        Ok(Response::new(PushMetricsResponse::default()))
    }

    async fn push_route_snapshot(
        &self,
        request: Request<RouteSnapshotRequest>,
    ) -> Result<Response<PushRouteSnapshotResponse>, Status> {
        self.counters
            .push_route_snapshots
            .lock()
            .expect("poisoned")
            .push(request.into_inner());
        Ok(Response::new(PushRouteSnapshotResponse::default()))
    }

    async fn get_config(
        &self,
        _request: Request<GetConfigRequest>,
    ) -> Result<Response<ConfigResponse>, Status> {
        Ok(Response::new(ConfigResponse {
            udp_probe_secret: vec![0u8; 8].into(),
            ..Default::default()
        }))
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
                tcp_probe_port: 3555,
                udp_probe_port: 3552,
            },
            Target {
                id: "peer-b".to_string(),
                ip: vec![10, 0, 0, 2].into(),
                display_name: "Peer B".to_string(),
                location: "Test".to_string(),
                lat: 3.0,
                lon: 4.0,
                tcp_probe_port: 3555,
                udp_probe_port: 3552,
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
        push_metrics_batches: StdMutex::new(Vec::new()),
        push_route_snapshots: StdMutex::new(Vec::new()),
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

    // Allocate ephemeral probe ports so the listeners don't collide with
    // parallel tests or other processes on well-known ports.
    let tcp_sock = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_probe_port = tcp_sock.local_addr().unwrap().port();
    drop(tcp_sock);
    let udp_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_probe_port = udp_sock.local_addr().unwrap().port();
    drop(udp_sock);

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
        tcp_probe_port,
        udp_probe_port,
        icmp_target_concurrency: 32,
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

/// End-to-end lifecycle smoke: the agent bootstraps, the real probers run
/// against the local loopback mock server, and the agent shuts down cleanly
/// after a paused 3-minute simulated window. Because probe samples arrive
/// non-deterministically in paused-clock time (they depend on prober wake
/// order + interval alignment), this test does NOT pin the exact number
/// of metric batches observed; that invariant is covered with deterministic
/// inputs by `emitter::tests::three_minute_session_emits_three_metric_batches`
/// where the emitter is driven directly from a hand-assembled channel.
///
/// What this test pins:
///   - bootstrap → register() called exactly once
///   - agent survives 3 min of paused time without panicking
///   - shutdown completes within its outer deadline
///   - any batches that DO arrive carry sane AgentMetadata
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn three_minute_session_completes_registration_and_shutdown_without_panic() {
    let (addr, counters) = start_mock_server().await;

    let tcp_sock = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_probe_port = tcp_sock.local_addr().unwrap().port();
    drop(tcp_sock);
    let udp_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_probe_port = udp_sock.local_addr().unwrap().port();
    drop(udp_sock);

    let env = AgentEnv {
        service_url: format!("http://{addr}"),
        agent_token: "test-token".to_string(),
        identity: AgentIdentity {
            id: "e2e-agent".to_string(),
            display_name: "E2E Agent".to_string(),
            location: "Test".to_string(),
            ip: "127.0.0.1".parse().unwrap(),
            lat: 0.0,
            lon: 0.0,
        },
        agent_version: "0.0.0-test".to_string(),
        tcp_probe_port,
        udp_probe_port,
        icmp_target_concurrency: 32,
    };

    let api = GrpcServiceApi::connect(&env.service_url, &env.agent_token)
        .await
        .expect("connect should succeed");

    let cancel = CancellationToken::new();
    let runtime = AgentRuntime::bootstrap(env, api, cancel.clone())
        .await
        .expect("bootstrap should succeed");

    // Drive 3 minutes of simulated time. Real probers run against the
    // loopback mock server; samples arrive and state machines classify,
    // so metric batches will be dispatched. Exact batch count depends on
    // prober wake alignment under the paused clock and is therefore only
    // sanity-checked here. Deterministic batch-cadence coverage lives in
    // `emitter::tests::three_minute_session_emits_three_metric_batches`.
    for _ in 0..36 {
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        // Yield aggressively so the tokio runtime lets each sub-task
        // (emitter, retry worker, every per-target supervisor) run.
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
    }

    assert_eq!(
        counters.register_count.load(Ordering::SeqCst),
        1,
        "register should have been called exactly once across the 3-minute window",
    );

    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(30), runtime.shutdown())
        .await
        .expect("shutdown should complete within 30 s");

    // Any batches that DID arrive must have plausible AgentMetadata.
    // We don't pin the count because the deterministic unit test
    // `emitter::tests::three_minute_session_emits_three_metric_batches`
    // covers batch cadence with injected inputs — here we only verify
    // that the live path round-trips valid metadata.
    let batches = counters
        .push_metrics_batches
        .lock()
        .expect("poisoned")
        .clone();
    for batch in &batches {
        let md = batch
            .agent_metadata
            .as_ref()
            .expect("AgentMetadata always stamped");
        assert_eq!(md.version, "0.0.0-test");
        assert_eq!(
            md.dropped_count, 0,
            "no overflow expected in a clean 3 min window"
        );
        assert!(
            md.uptime_secs <= 300,
            "uptime_secs {} should not exceed 5 min window",
            md.uptime_secs,
        );
    }

    // Same sanity check for route snapshots: if any happen to fire under
    // the real probers, they must carry a valid source_id + path_summary.
    let snapshots = counters
        .push_route_snapshots
        .lock()
        .expect("poisoned")
        .clone();
    for snap in &snapshots {
        assert_eq!(snap.source_id, "e2e-agent");
        assert!(snap.path_summary.is_some(), "path_summary must be stamped");
    }
}
