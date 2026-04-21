//! Integration tests for the dispatch-origin campaign metrics
//! (`meshmon_campaign_pairs_inflight`,
//! `meshmon_campaign_dest_bucket_wait_seconds`,
//! `meshmon_campaign_probe_collisions_total`).
//!
//! Drives a real `RpcDispatcher` call against a loopback tonic server
//! and scrapes the rendered Prometheus exposition to confirm each new
//! metric is wired end-to-end.
//!
//! The Prometheus recorder is process-global and shared with every
//! other integration test in this crate (via
//! `common::test_prometheus_handle`). Each assertion therefore checks
//! for the presence of HELP/TYPE metadata and at least one sample line
//! with the expected label set — not an exact counter value, since
//! parallel tests may have bumped the underlying series.

#[path = "common/mod.rs"]
mod common;

use futures_util::Stream;
use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{
    AgentCommand, AgentCommandServer, MeasurementResult, MeasurementSummary, RefreshConfigRequest,
    RefreshConfigResponse, RunMeasurementBatchRequest,
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

/// Test-unique destination IP from TEST-NET-3.
fn unique_dest() -> IpAddr {
    let n = DEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    IpAddr::from([198, 51, 100, (n % 254 + 1) as u8])
}

type ResultStream = Pin<Box<dyn Stream<Item = Result<MeasurementResult, Status>> + Send + 'static>>;

/// Echoes every target as a successful measurement — identical to the
/// agent used in `campaigns_rpc_dispatcher.rs`, duplicated here so this
/// file stays self-contained.
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
            title: "dispatch-metrics".into(),
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
            kind: r.kind,
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

/// `meshmon_campaign_dest_bucket_wait_seconds` is a histogram with no
/// labels. After a successful dispatch the exposition must include a
/// `_count` line with at least one observation.
#[tokio::test]
async fn dispatch_records_dest_bucket_wait_seconds_histogram() {
    let handle = common::test_prometheus_handle().await;
    let pool = common::shared_migrated_pool().await;

    let agent_id = "agent-metrics-bucket";
    seed_agent_row(&pool, agent_id, "203.0.113.40").await;
    let registry = make_registry(&pool).await;

    let channel = spawn_agent_server(EchoSuccessAgent).await;
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![(
        agent_id.to_string(),
        channel,
    )]));
    let writer = SettleWriter::new(pool.clone());
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 100, 50);

    let (campaign_id, pending) = seed_dispatched_batch(&pool, agent_id, vec![unique_dest()]).await;
    let outcome = dispatcher.dispatch(agent_id, pending).await;
    assert_eq!(outcome.dispatched, 1);

    let body = handle.render();
    assert!(
        body.contains("# HELP meshmon_campaign_dest_bucket_wait_seconds"),
        "missing HELP for dest_bucket_wait_seconds in:\n{body}"
    );
    assert!(
        body.contains("# TYPE meshmon_campaign_dest_bucket_wait_seconds histogram"),
        "missing histogram TYPE for dest_bucket_wait_seconds in:\n{body}"
    );
    assert!(
        body.lines()
            .any(|l| l.starts_with("meshmon_campaign_dest_bucket_wait_seconds_count")),
        "missing _count sample for dest_bucket_wait_seconds in:\n{body}"
    );

    repo::delete(&pool, campaign_id).await.expect("cleanup");
}

/// `meshmon_campaign_pairs_inflight{agent=...}` must appear with a
/// value of 0 after the dispatcher returns (inc on entry, dec on drop).
#[tokio::test]
async fn dispatch_bumps_and_releases_pairs_inflight_gauge() {
    let handle = common::test_prometheus_handle().await;
    let pool = common::shared_migrated_pool().await;

    let agent_id = "agent-metrics-inflight";
    seed_agent_row(&pool, agent_id, "203.0.113.41").await;
    let registry = make_registry(&pool).await;

    let channel = spawn_agent_server(EchoSuccessAgent).await;
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![(
        agent_id.to_string(),
        channel,
    )]));
    let writer = SettleWriter::new(pool.clone());
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 100, 50);

    let (campaign_id, pending) = seed_dispatched_batch(&pool, agent_id, vec![unique_dest()]).await;
    let outcome = dispatcher.dispatch(agent_id, pending).await;
    assert_eq!(outcome.dispatched, 1);

    let body = handle.render();
    assert!(
        body.contains("# HELP meshmon_campaign_pairs_inflight"),
        "missing HELP for pairs_inflight in:\n{body}"
    );
    assert!(
        body.contains("# TYPE meshmon_campaign_pairs_inflight gauge"),
        "missing gauge TYPE for pairs_inflight in:\n{body}"
    );
    // After dispatch completes the guard drops and the gauge returns to 0.
    let line = body
        .lines()
        .find(|l| {
            l.starts_with("meshmon_campaign_pairs_inflight{")
                && l.contains(&format!(r#"agent="{agent_id}""#))
        })
        .unwrap_or_else(|| panic!("missing pairs_inflight line for {agent_id} in:\n{body}"));
    let value: f64 = line
        .rsplit(' ')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("could not parse inflight value from: {line}"));
    assert_eq!(
        value, 0.0,
        "pairs_inflight must return to zero once dispatch completes; line: {line}"
    );

    repo::delete(&pool, campaign_id).await.expect("cleanup");
}

/// `meshmon_campaign_probe_collisions_total` is a label-less counter
/// seeded at 0 by `main.rs`. The test forces one increment so the
/// metric surfaces in the scrape and asserts the HELP/TYPE contract is
/// correct.
#[tokio::test]
async fn probe_collisions_counter_has_help_and_type() {
    let handle = common::test_prometheus_handle().await;

    // Seed with `absolute(0)` — the same call `main.rs` makes at boot.
    // Prometheus exporters surface the series only after at least one
    // operation, so use `increment(0)` here too to force registration
    // without changing the displayed value across test binaries.
    meshmon_service::metrics::campaign_probe_collisions_total().absolute(0);
    meshmon_service::metrics::campaign_probe_collisions_total().increment(0);

    let body = handle.render();
    assert!(
        body.contains("# HELP meshmon_campaign_probe_collisions_total"),
        "missing HELP for probe_collisions_total in:\n{body}"
    );
    assert!(
        body.contains("# TYPE meshmon_campaign_probe_collisions_total counter"),
        "missing counter TYPE for probe_collisions_total in:\n{body}"
    );
    // Label-less counter; the sample line is the metric name followed
    // by one or more spaces and the value.
    assert!(
        body.lines().any(|l| {
            l.starts_with("meshmon_campaign_probe_collisions_total")
                && !l.starts_with("# ")
                && l.split_whitespace().count() == 2
        }),
        "missing probe_collisions_total sample line in:\n{body}"
    );
}
