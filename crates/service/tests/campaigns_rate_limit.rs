//! Cross-agent rate-limit test.
//!
//! Two `RpcDispatcher::dispatch` calls — one per source agent, both
//! targeting the same destination IP — must share a single process-wide
//! token bucket. With `per_destination_rps = 1`, the combined number of
//! dispatched pairs across both agents must not exceed 1 per second.
//!
//! If the bucket were mistakenly per-agent, both agents would each
//! successfully dispatch their pair and `combined == 2`.

#[path = "common/mod.rs"]
mod common;

use meshmon_agent::command::{AgentCommandService, StubProber};
use meshmon_protocol::AgentCommandServer;
use meshmon_revtunnel::TunnelManager;
use meshmon_service::campaign::dispatch::{PairDispatcher, PendingPair};
use meshmon_service::campaign::model::ProbeProtocol;
use meshmon_service::campaign::repo::{self, CreateInput};
use meshmon_service::campaign::rpc_dispatcher::RpcDispatcher;
use meshmon_service::campaign::writer::SettleWriter;
use meshmon_service::registry::AgentRegistry;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tonic::transport::{Channel, Endpoint, Server};

/// Boot a loopback `AgentCommandServer` backed by `StubProber` and
/// eagerly-connect a channel so the rate-limit measurement below does
/// not include TCP handshake latency.
async fn spawn_stub() -> (SocketAddr, Channel) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let svc = AgentCommandService::new(Arc::new(Notify::new()), Arc::new(StubProber), 16);
    tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(AgentCommandServer::new(svc))
            .serve_with_incoming(incoming)
            .await;
    });
    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .expect("endpoint")
        .connect_timeout(Duration::from_secs(2))
        .connect()
        .await
        .expect("connect");
    (addr, channel)
}

#[tokio::test]
async fn per_destination_rps_caps_cross_agent_traffic() {
    let pool = common::shared_migrated_pool().await;

    let agent_x = "agent-ratelim-x";
    let agent_y = "agent-ratelim-y";

    // Two agent rows registered to the cluster. Both are used as
    // `source_agent_id` for pairs pointed at the same destination IP
    // so the dispatcher's per-destination bucket is the shared limiter.
    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, last_seen_at) \
         VALUES ($1, $1, '198.51.100.201'::inet, 7000, 7001, now()), \
                ($2, $2, '198.51.100.202'::inet, 7000, 7001, now()) \
         ON CONFLICT (id) DO UPDATE SET last_seen_at = now()",
    )
    .bind(agent_x)
    .bind(agent_y)
    .execute(&pool)
    .await
    .expect("seed agents");
    let registry = Arc::new(AgentRegistry::new(
        pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.expect("registry load");

    let (_addr_x, chan_x) = spawn_stub().await;
    let (_addr_y, chan_y) = spawn_stub().await;
    let tunnels = Arc::new(TunnelManager::new_for_test(vec![
        (agent_x.to_string(), chan_x),
        (agent_y.to_string(), chan_y),
    ]));
    let writer = SettleWriter::new(pool.clone());

    // Cap: 1 dispatched pair per destination per second, cluster-wide.
    // One agent's pair draws the only token; the other agent's pair
    // MUST land in `rejected_ids`. If the bucket were per-agent, both
    // would succeed and the assertion below would fail.
    let dispatcher = RpcDispatcher::new(tunnels, registry, writer, 16, 1, 50);

    // A single campaign with one destination and both agents as
    // sources — creates 2 pairs, one per (source, dest).
    let dest = IpAddr::from_str("198.51.100.150").expect("parse dest");
    let campaign = repo::create(
        &pool,
        CreateInput {
            title: "rate-limit".into(),
            notes: "".into(),
            protocol: ProbeProtocol::Icmp,
            source_agent_ids: vec![agent_x.to_string(), agent_y.to_string()],
            destination_ips: vec![dest],
            force_measurement: true,
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
    repo::start(&pool, campaign.id).await.expect("start");

    // Promote every pair to `dispatched` so the writer's state gate
    // accepts the stub-prober settlements. Simulates the state
    // `take_pending_batch` would have left them in.
    sqlx::query(
        "UPDATE campaign_pairs \
            SET resolution_state = 'dispatched', dispatched_at = now() \
          WHERE campaign_id = $1",
    )
    .bind(campaign.id)
    .execute(&pool)
    .await
    .expect("flip to dispatched");

    let all_pairs: Vec<(i64, String, sqlx::types::ipnetwork::IpNetwork)> = sqlx::query_as(
        "SELECT id, source_agent_id, destination_ip \
           FROM campaign_pairs WHERE campaign_id = $1",
    )
    .bind(campaign.id)
    .fetch_all(&pool)
    .await
    .expect("select pairs");
    assert_eq!(all_pairs.len(), 2, "expected one pair per source agent");

    let build_pair = |id: i64, agent: &str| PendingPair {
        pair_id: id,
        campaign_id: campaign.id,
        source_agent_id: agent.to_string(),
        destination_ip: dest,
        probe_count: 10,
        timeout_ms: 2_000,
        probe_stagger_ms: 100,
        force_measurement: true,
        protocol: ProbeProtocol::Icmp,
    };

    let pairs_x: Vec<PendingPair> = all_pairs
        .iter()
        .filter(|(_, a, _)| a == agent_x)
        .map(|(id, _, _)| build_pair(*id, agent_x))
        .collect();
    let pairs_y: Vec<PendingPair> = all_pairs
        .iter()
        .filter(|(_, a, _)| a == agent_y)
        .map(|(id, _, _)| build_pair(*id, agent_y))
        .collect();

    // Fire both dispatchers in parallel. Both sides share the same
    // `RpcDispatcher` instance, so they share the destination bucket.
    let (out_x, out_y) = tokio::join!(
        dispatcher.dispatch(agent_x, pairs_x),
        dispatcher.dispatch(agent_y, pairs_y),
    );

    let combined_dispatched = out_x.dispatched + out_y.dispatched;
    let combined_rejected = out_x.rejected_ids.len() + out_y.rejected_ids.len();

    // The bucket starts full at capacity 1. Exactly one dispatch can
    // succeed in the first second; the other must be rejected with
    // `rate_limited`. If the buckets were per-agent, both would
    // succeed and `combined_dispatched == 2` would break the test.
    assert_eq!(
        combined_dispatched, 1,
        "expected exactly 1 pair through the shared bucket, got {combined_dispatched} \
         (dispatched: x={}, y={}; rejected: x={:?}, y={:?})",
        out_x.dispatched, out_y.dispatched, out_x.rejected_ids, out_y.rejected_ids,
    );
    assert_eq!(
        combined_rejected, 1,
        "expected the other pair to be rejected by the rate limiter"
    );

    // One of the two agents emits `skipped_reason = Some("rate_limited")`
    // for its single rate-capped pair. (The other agent's call returns
    // `skipped_reason = None` because some of its pairs DID dispatch.)
    let rate_limited_marker = out_x.skipped_reason.as_deref() == Some("rate_limited")
        || out_y.skipped_reason.as_deref() == Some("rate_limited");
    assert!(
        rate_limited_marker,
        "one of the rejected dispatches must carry skipped_reason=rate_limited \
         (x={:?}, y={:?})",
        out_x.skipped_reason, out_y.skipped_reason,
    );

    repo::delete(&pool, campaign.id).await.expect("cleanup");
    sqlx::query("DELETE FROM agents WHERE id IN ($1, $2)")
        .bind(agent_x)
        .bind(agent_y)
        .execute(&pool)
        .await
        .expect("cleanup agents");
}
