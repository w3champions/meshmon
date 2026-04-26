//! End-to-end campaign dispatch test.
//!
//! Wires the full T45 pipeline into a single test binary:
//!
//! ```text
//!                      ┌──────────────────┐
//!  tonic loopback  ◄───┤ AgentCommandSvc   │  (StubProber — deterministic
//!  (127.0.0.1:RNG)     │  + StubProber     │   success per target)
//!                      └────────▲─────────┘
//!                               │
//!            channel_for("agent-a") ── TunnelManager::new_for_test
//!                               │
//!                      ┌────────┴─────────┐
//!  scheduler.run() ───►│   RpcDispatcher  │──► SettleWriter ──► Postgres
//!                      └──────────────────┘                 ──► NOTIFY
//!                                                                 │
//!                      scheduler.PgListener  ◄────────────────────┘
//! ```
//!
//! The scheduler subscribes to both `campaign_state_changed` and
//! `campaign_pair_settled` so writer-side settlements wake the loop and
//! `repo::maybe_complete` transitions the campaign to `completed`.

#[path = "common/mod.rs"]
mod common;

use meshmon_agent::command::{AgentCommandService, StubProber};
use meshmon_protocol::AgentCommandServer;
use meshmon_revtunnel::TunnelManager;
use meshmon_service::campaign::dispatch::PairDispatcher;
use meshmon_service::campaign::model::ProbeProtocol;
use meshmon_service::campaign::repo::{self, CreateInput};
use meshmon_service::campaign::rpc_dispatcher::RpcDispatcher;
use meshmon_service::campaign::scheduler::Scheduler;
use meshmon_service::campaign::writer::SettleWriter;
use meshmon_service::registry::AgentRegistry;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Endpoint, Server};

#[tokio::test]
async fn scheduler_dispatches_five_batches_end_to_end() {
    let pool = common::shared_migrated_pool().await;

    // Spin up a loopback tonic server hosting AgentCommandService+StubProber.
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

    // Eager connect — a lazy Channel would race the RPC against the
    // server's own setup when the scheduler fires its first tick.
    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .expect("endpoint")
        .connect_timeout(Duration::from_secs(2))
        .connect()
        .await
        .expect("connect to fake agent");

    // Seed the agent row so the registry picks it up as active. Use a
    // test-unique agent id — the shared DB is process-wide and other
    // binaries may leave `agent-a` rows behind.
    let agent_id = "agent-e2e-dispatch";
    let agent_ip = "198.51.100.50";
    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, last_seen_at) \
         VALUES ($1, $1, $2::inet, 7000, 7001, now()) \
         ON CONFLICT (id) DO UPDATE SET last_seen_at = now()",
    )
    .bind(agent_id)
    .bind(agent_ip)
    .execute(&pool)
    .await
    .expect("seed agent");

    let registry = Arc::new(AgentRegistry::new(
        pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.expect("registry load");

    let tunnels = Arc::new(TunnelManager::new_for_test(vec![(
        agent_id.to_string(),
        channel,
    )]));
    let writer = SettleWriter::new(pool.clone());
    let dispatcher: Arc<dyn PairDispatcher> = Arc::new(RpcDispatcher::new(
        tunnels,
        registry.clone(),
        writer,
        16,
        1_000,
        50,
    ));

    // Seed a campaign with five destinations. `force_measurement=true`
    // disables the 24 h reuse cache so the scheduler actually routes
    // every pair through the dispatcher.
    let destinations: Vec<IpAddr> = (0..5)
        .map(|i| IpAddr::from_str(&format!("198.51.100.{}", 100 + i)).expect("parse dest"))
        .collect();
    let campaign = repo::create(
        &pool,
        CreateInput {
            title: "e2e".into(),
            notes: "".into(),
            protocol: ProbeProtocol::Icmp,
            source_agent_ids: vec![agent_id.to_string()],
            destination_ips: destinations,
            force_measurement: true,
            probe_count: None,
            probe_count_detail: None,
            timeout_ms: None,
            probe_stagger_ms: None,
            loss_threshold_ratio: None,
            stddev_weight: None,
            evaluation_mode: None,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms: None,
            max_hops: None,
            vm_lookback_minutes: None,
            created_by: None,
        },
    )
    .await
    .expect("create campaign");
    repo::start(&pool, campaign.id).await.expect("start");

    // Run the scheduler for real. `tick_ms = 100` plus the
    // `campaign_pair_settled` NOTIFY bound keeps the loop responsive
    // even when five writer settlements race the 100 ms fallback.
    let cancel = CancellationToken::new();
    let scheduler = Scheduler::new(
        pool.clone(),
        registry,
        dispatcher,
        100,
        10,
        3,
        Duration::from_secs(300),
    );
    let handle = tokio::spawn(scheduler.run(cancel.clone()));

    // Poll until every pair has a terminal `succeeded` state or the
    // deadline fires. The stub prober returns a success per target, so
    // any other terminal state indicates a regression. The 10 s window
    // is deliberately generous — test binaries that share the Postgres
    // pool can occasionally queue a few hundred ms behind other DML
    // traffic, and shaving the budget too tight risks flakes on CI.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let succeeded: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint \
               FROM campaign_pairs \
              WHERE campaign_id = $1 AND resolution_state = 'succeeded'",
        )
        .bind(campaign.id)
        .fetch_one(&pool)
        .await
        .expect("count pairs");
        if succeeded == 5 {
            break;
        }
        if std::time::Instant::now() > deadline {
            let debug: Vec<(i64, String)> = sqlx::query_as(
                "SELECT id, resolution_state::text \
                   FROM campaign_pairs WHERE campaign_id = $1",
            )
            .bind(campaign.id)
            .fetch_all(&pool)
            .await
            .expect("debug query");
            panic!("only {succeeded}/5 pairs succeeded after 10s; states = {debug:?}",);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // `maybe_complete` fires on the scheduler's next tick after the
    // last settle — give it a short window to land.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let final_state = loop {
        let state: String =
            sqlx::query_scalar("SELECT state::text FROM measurement_campaigns WHERE id = $1")
                .bind(campaign.id)
                .fetch_one(&pool)
                .await
                .expect("read state");
        if state == "completed" {
            break state;
        }
        if std::time::Instant::now() > deadline {
            break state;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        final_state, "completed",
        "scheduler did not transition campaign to completed"
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;

    repo::delete(&pool, campaign.id).await.expect("cleanup");
    sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(agent_id)
        .execute(&pool)
        .await
        .expect("cleanup agent");
}
