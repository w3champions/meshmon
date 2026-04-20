//! Integration tests for `RpcDispatcher` against a loopback tonic
//! server.
//!
//! Seeds a dispatched campaign pair, wires a loopback
//! `AgentCommandServer` into a `TunnelManager::new_for_test`-backed
//! registry, and asserts on what the dispatcher writes.

#[path = "common/mod.rs"]
mod common;

use futures_util::Stream;
use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{
    AgentCommand, AgentCommandServer, MeasurementFailure, MeasurementFailureCode,
    MeasurementResult, MeasurementSummary, RefreshConfigRequest, RefreshConfigResponse,
    RunMeasurementBatchRequest,
};
use meshmon_revtunnel::TunnelManager;
use meshmon_service::campaign::dispatch::{PairDispatcher, PendingPair};
use meshmon_service::campaign::model::ProbeProtocol;
use meshmon_service::campaign::repo::{self, CreateInput};
use meshmon_service::campaign::rpc_dispatcher::RpcDispatcher;
use meshmon_service::campaign::writer::SettleWriter;
use meshmon_service::registry::AgentRegistry;
use sqlx::PgPool;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tonic::transport::{Channel, Endpoint, Server};
use tonic::{Request, Response, Status};

static DEST_COUNTER: AtomicU32 = AtomicU32::new(1);

/// Allocate a test-unique destination IP from TEST-NET-3 (RFC 5737).
fn unique_dest() -> IpAddr {
    let n = DEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    IpAddr::from([198, 51, 100, (n % 254 + 1) as u8])
}

// --- Fake agent implementations ------------------------------------------------

type ResultStream = Pin<Box<dyn Stream<Item = Result<MeasurementResult, Status>> + Send + 'static>>;

/// Echoes every target as a successful measurement.
struct EchoSuccessAgent;

#[tonic::async_trait]
impl AgentCommand for EchoSuccessAgent {
    async fn refresh_config(
        &self,
        _: Request<RefreshConfigRequest>,
    ) -> Result<Response<RefreshConfigResponse>, Status> {
        Ok(Response::new(RefreshConfigResponse {}))
    }

    type RunMeasurementBatchStream = ResultStream;

    async fn run_measurement_batch(
        &self,
        req: Request<RunMeasurementBatchRequest>,
    ) -> Result<Response<Self::RunMeasurementBatchStream>, Status> {
        let req = req.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(req.targets.len().max(1));
        for t in req.targets {
            let r = MeasurementResult {
                pair_id: t.pair_id,
                outcome: Some(Outcome::Success(MeasurementSummary {
                    attempted: req.probe_count,
                    succeeded: req.probe_count,
                    latency_min_ms: 1.0,
                    latency_avg_ms: 1.0,
                    latency_median_ms: 1.0,
                    latency_p95_ms: 1.0,
                    latency_max_ms: 1.0,
                    latency_stddev_ms: 0.0,
                    loss_pct: 0.0,
                })),
            };
            let _ = tx.send(Ok(r)).await;
        }
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream)))
    }
}

/// Returns a `NoRoute` failure for every target — exercises the
/// writer's `unreachable` path via the dispatcher.
struct FailingAgent {
    code: MeasurementFailureCode,
}

#[tonic::async_trait]
impl AgentCommand for FailingAgent {
    async fn refresh_config(
        &self,
        _: Request<RefreshConfigRequest>,
    ) -> Result<Response<RefreshConfigResponse>, Status> {
        Ok(Response::new(RefreshConfigResponse {}))
    }

    type RunMeasurementBatchStream = ResultStream;

    async fn run_measurement_batch(
        &self,
        req: Request<RunMeasurementBatchRequest>,
    ) -> Result<Response<Self::RunMeasurementBatchStream>, Status> {
        let req = req.into_inner();
        let code = self.code;
        let (tx, rx) = tokio::sync::mpsc::channel(req.targets.len().max(1));
        for t in req.targets {
            let r = MeasurementResult {
                pair_id: t.pair_id,
                outcome: Some(Outcome::Failure(MeasurementFailure {
                    code: code as i32,
                    detail: format!("{code:?}"),
                })),
            };
            let _ = tx.send(Ok(r)).await;
        }
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream)))
    }
}

/// Drops half the results — used to exercise the stream-drop path.
/// Sends exactly `send_count` successful results then closes the
/// stream before the agent would have delivered the rest.
struct TruncatedAgent {
    send_count: usize,
}

#[tonic::async_trait]
impl AgentCommand for TruncatedAgent {
    async fn refresh_config(
        &self,
        _: Request<RefreshConfigRequest>,
    ) -> Result<Response<RefreshConfigResponse>, Status> {
        Ok(Response::new(RefreshConfigResponse {}))
    }

    type RunMeasurementBatchStream = ResultStream;

    async fn run_measurement_batch(
        &self,
        req: Request<RunMeasurementBatchRequest>,
    ) -> Result<Response<Self::RunMeasurementBatchStream>, Status> {
        let req = req.into_inner();
        let limit = self.send_count;
        let (tx, rx) = tokio::sync::mpsc::channel(req.targets.len().max(1));
        for (i, t) in req.targets.into_iter().enumerate() {
            if i >= limit {
                break;
            }
            let r = MeasurementResult {
                pair_id: t.pair_id,
                outcome: Some(Outcome::Success(MeasurementSummary {
                    attempted: req.probe_count,
                    succeeded: req.probe_count,
                    latency_min_ms: 1.0,
                    latency_avg_ms: 1.0,
                    latency_median_ms: 1.0,
                    latency_p95_ms: 1.0,
                    latency_max_ms: 1.0,
                    latency_stddev_ms: 0.0,
                    loss_pct: 0.0,
                })),
            };
            let _ = tx.send(Ok(r)).await;
        }
        drop(tx);
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream)))
    }
}

// --- Harness helpers ---------------------------------------------------------

async fn spawn_agent_server<A>(agent: A) -> Channel
where
    A: AgentCommand + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(AgentCommandServer::new(agent))
            .serve_with_incoming(incoming)
            .await;
    });
    // Eager connect — a lazy Channel would race the RPC against the
    // server's own setup in the rate-limit test.
    Endpoint::from_shared(format!("http://{addr}"))
        .expect("endpoint")
        .connect_timeout(Duration::from_secs(2))
        .connect()
        .await
        .expect("connect to fake agent")
}

async fn seed_agent_row(pool: &PgPool, agent_id: &str, ip: &str) {
    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, last_seen_at) \
         VALUES ($1, $1, $2::inet, 7000, 7001, NOW()) \
         ON CONFLICT (id) DO UPDATE SET last_seen_at = NOW()",
    )
    .bind(agent_id)
    .bind(ip)
    .execute(pool)
    .await
    .expect("insert agent");
}

async fn seed_dispatched_batch(
    pool: &PgPool,
    agent_id: &str,
    destinations: Vec<IpAddr>,
) -> (uuid::Uuid, Vec<PendingPair>) {
    let campaign = repo::create(
        pool,
        CreateInput {
            title: "rpc-disp".into(),
            notes: "".into(),
            protocol: ProbeProtocol::Icmp,
            source_agent_ids: vec![agent_id.to_string()],
            destination_ips: destinations,
            force_measurement: false,
            probe_count: None,
            probe_count_detail: None,
            timeout_ms: None,
            probe_stagger_ms: None,
            loss_threshold_pct: None,
            stddev_weight: None,
            evaluation_mode: None,
            created_by: None,
        },
    )
    .await
    .expect("create campaign");
    repo::start(pool, campaign.id).await.expect("start");
    let batch = repo::take_pending_batch(pool, campaign.id, agent_id, 100)
        .await
        .expect("take batch");
    let pending = batch
        .into_iter()
        .map(|r| PendingPair {
            pair_id: r.id,
            campaign_id: campaign.id,
            source_agent_id: r.source_agent_id,
            destination_ip: match r.destination_ip {
                sqlx::types::ipnetwork::IpNetwork::V4(n) => IpAddr::V4(n.ip()),
                sqlx::types::ipnetwork::IpNetwork::V6(n) => IpAddr::V6(n.ip()),
            },
            probe_count: campaign.probe_count,
            timeout_ms: campaign.timeout_ms,
            probe_stagger_ms: campaign.probe_stagger_ms,
            force_measurement: campaign.force_measurement,
            protocol: campaign.protocol,
        })
        .collect();
    (campaign.id, pending)
}

async fn make_registry(pool: &PgPool) -> Arc<AgentRegistry> {
    let r = Arc::new(AgentRegistry::new(
        pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    r.initial_load().await.expect("initial load");
    r
}

/// Read resolution state for one pair as a `String` for stable equality
/// assertions against Postgres enum literals.
async fn pair_state(pool: &PgPool, pair_id: i64) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT resolution_state::text FROM campaign_pairs WHERE id = $1",
    )
    .bind(pair_id)
    .fetch_one(pool)
    .await
    .expect("read pair state")
}

// --- Tests -------------------------------------------------------------------

#[tokio::test]
async fn dispatch_drains_stream_and_flips_pair_to_succeeded() {
    let pool = common::shared_migrated_pool().await;
    let agent_id = "agent-rpc-success";
    seed_agent_row(&pool, agent_id, "203.0.113.10").await;
    let registry = make_registry(&pool).await;

    let channel = spawn_agent_server(EchoSuccessAgent).await;
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![(
        agent_id.to_string(),
        channel,
    )]));

    let writer = SettleWriter::new(pool.clone());
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 100, 50);

    let (campaign_id, pending) = seed_dispatched_batch(&pool, agent_id, vec![unique_dest()]).await;
    assert_eq!(pending.len(), 1);
    let pair_id = pending[0].pair_id;

    let outcome = dispatcher.dispatch(agent_id, pending).await;
    assert_eq!(outcome.dispatched, 1);
    assert!(outcome.rejected_ids.is_empty());
    assert!(outcome.skipped_reason.is_none());

    assert_eq!(pair_state(&pool, pair_id).await, "succeeded");

    repo::delete(&pool, campaign_id).await.expect("cleanup");
}

#[tokio::test]
async fn dispatch_missing_tunnel_rejects_every_pair_with_agent_unreachable() {
    let pool = common::shared_migrated_pool().await;
    let agent_id = "agent-no-tunnel";
    seed_agent_row(&pool, agent_id, "203.0.113.11").await;
    let registry = make_registry(&pool).await;

    // Empty tunnel registry — no channel for this agent.
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![]));
    let writer = SettleWriter::new(pool.clone());
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 100, 50);

    let (campaign_id, pending) = seed_dispatched_batch(&pool, agent_id, vec![unique_dest()]).await;
    let pair_id = pending[0].pair_id;
    let outcome = dispatcher.dispatch(agent_id, pending).await;
    assert_eq!(outcome.dispatched, 0);
    assert_eq!(outcome.rejected_ids, vec![pair_id]);
    assert_eq!(outcome.skipped_reason.as_deref(), Some("agent_unreachable"));

    // The pair is left in `dispatched` — the scheduler reverts it to
    // `pending` via the `rejected_ids` path.
    assert_eq!(pair_state(&pool, pair_id).await, "dispatched");
    repo::delete(&pool, campaign_id).await.expect("cleanup");
}

#[tokio::test]
async fn dispatch_failure_code_settles_via_writer_without_rejection() {
    let pool = common::shared_migrated_pool().await;
    let agent_id = "agent-rpc-noroute";
    seed_agent_row(&pool, agent_id, "203.0.113.12").await;
    let registry = make_registry(&pool).await;

    let channel = spawn_agent_server(FailingAgent {
        code: MeasurementFailureCode::NoRoute,
    })
    .await;
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![(
        agent_id.to_string(),
        channel,
    )]));
    let writer = SettleWriter::new(pool.clone());
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 100, 50);

    let (campaign_id, pending) = seed_dispatched_batch(&pool, agent_id, vec![unique_dest()]).await;
    let pair_id = pending[0].pair_id;

    let outcome = dispatcher.dispatch(agent_id, pending).await;
    // `MeasurementFailure` is a settled result — the writer maps
    // `NoRoute` to `unreachable` and the dispatcher counts one dispatch.
    assert_eq!(outcome.dispatched, 1);
    assert!(
        outcome.rejected_ids.is_empty(),
        "terminal failure must settle via writer, not rejected_ids: {:?}",
        outcome.rejected_ids
    );
    assert_eq!(pair_state(&pool, pair_id).await, "unreachable");

    repo::delete(&pool, campaign_id).await.expect("cleanup");
}

#[tokio::test]
async fn dispatch_stream_drop_rejects_undelivered_pairs() {
    let pool = common::shared_migrated_pool().await;
    let agent_id = "agent-rpc-truncate";
    seed_agent_row(&pool, agent_id, "203.0.113.13").await;
    let registry = make_registry(&pool).await;

    // Agent sends one result then drops. Dispatcher must settle the
    // delivered pair and reject the other.
    let channel = spawn_agent_server(TruncatedAgent { send_count: 1 }).await;
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![(
        agent_id.to_string(),
        channel,
    )]));
    let writer = SettleWriter::new(pool.clone());
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 100, 50);

    let (campaign_id, pending) =
        seed_dispatched_batch(&pool, agent_id, vec![unique_dest(), unique_dest()]).await;
    assert_eq!(pending.len(), 2);
    let outcome = dispatcher.dispatch(agent_id, pending).await;

    assert_eq!(outcome.dispatched, 1);
    assert_eq!(outcome.rejected_ids.len(), 1);
    assert!(
        outcome.skipped_reason.is_none(),
        "some results landed — whole-batch reason must stay None (got {:?})",
        outcome.skipped_reason
    );
    repo::delete(&pool, campaign_id).await.expect("cleanup");
}

#[tokio::test]
async fn dispatch_rate_limit_rejects_every_pair_when_rps_is_zero() {
    let pool = common::shared_migrated_pool().await;
    let agent_id = "agent-rpc-ratelim";
    seed_agent_row(&pool, agent_id, "203.0.113.14").await;
    let registry = make_registry(&pool).await;

    // The agent will never be called because the rate limiter rejects
    // every pair up-front — still wire a real channel so the tunnel
    // lookup succeeds.
    let channel = spawn_agent_server(EchoSuccessAgent).await;
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![(
        agent_id.to_string(),
        channel,
    )]));
    let writer = SettleWriter::new(pool.clone());
    // `per_destination_rps = 0` — bucket capacity zero, every draw fails.
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 0, 50);

    let (campaign_id, pending) = seed_dispatched_batch(&pool, agent_id, vec![unique_dest()]).await;
    let pair_id = pending[0].pair_id;
    let outcome = dispatcher.dispatch(agent_id, pending).await;
    assert_eq!(outcome.dispatched, 0);
    assert_eq!(outcome.rejected_ids, vec![pair_id]);
    assert_eq!(outcome.skipped_reason.as_deref(), Some("rate_limited"));
    repo::delete(&pool, campaign_id).await.expect("cleanup");
}

/// Agent that sends one `MeasurementResult` with `outcome = None` —
/// a protocol violation. The dispatcher must reject the pair so the
/// scheduler reverts it; silent dropping would leave the pair stuck
/// in `dispatched` forever and block campaign completion.
struct MalformedOutcomeAgent;

#[tonic::async_trait]
impl AgentCommand for MalformedOutcomeAgent {
    async fn refresh_config(
        &self,
        _: Request<RefreshConfigRequest>,
    ) -> Result<Response<RefreshConfigResponse>, Status> {
        Ok(Response::new(RefreshConfigResponse {}))
    }

    type RunMeasurementBatchStream = ResultStream;

    async fn run_measurement_batch(
        &self,
        req: Request<RunMeasurementBatchRequest>,
    ) -> Result<Response<Self::RunMeasurementBatchStream>, Status> {
        let req = req.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(req.targets.len().max(1));
        for t in req.targets {
            let r = MeasurementResult {
                pair_id: t.pair_id,
                outcome: None,
            };
            let _ = tx.send(Ok(r)).await;
        }
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream)))
    }
}

#[tokio::test]
async fn dispatch_malformed_outcome_rejects_pair_and_leaves_state_dispatched() {
    let pool = common::shared_migrated_pool().await;
    let agent_id = "agent-rpc-malformed";
    seed_agent_row(&pool, agent_id, "203.0.113.15").await;
    let registry = make_registry(&pool).await;

    let channel = spawn_agent_server(MalformedOutcomeAgent).await;
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![(
        agent_id.to_string(),
        channel,
    )]));
    let writer = SettleWriter::new(pool.clone());
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 100, 50);

    let (campaign_id, pending) = seed_dispatched_batch(&pool, agent_id, vec![unique_dest()]).await;
    let pair_id = pending[0].pair_id;

    let outcome = dispatcher.dispatch(agent_id, pending).await;
    assert_eq!(outcome.dispatched, 0);
    assert_eq!(
        outcome.rejected_ids,
        vec![pair_id],
        "malformed outcome must revert the pair via rejected_ids",
    );
    // Writer rolled back the tx, so the pair stayed in `dispatched`;
    // the scheduler will flip it back to `pending` via `rejected_ids`.
    assert_eq!(pair_state(&pool, pair_id).await, "dispatched");

    repo::delete(&pool, campaign_id).await.expect("cleanup");
}

#[tokio::test]
async fn dispatch_empty_batch_returns_default_outcome() {
    let pool = common::shared_migrated_pool().await;
    let registry = make_registry(&pool).await;
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![]));
    let writer = SettleWriter::new(pool.clone());
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 100, 50);

    let outcome = dispatcher.dispatch("nobody", Vec::new()).await;
    assert_eq!(outcome.dispatched, 0);
    assert!(outcome.rejected_ids.is_empty());
    assert!(outcome.skipped_reason.is_none());
}
