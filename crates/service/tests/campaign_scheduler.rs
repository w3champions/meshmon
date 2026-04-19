//! Scheduler integration tests.
//!
//! Every test here uses [`common::own_container`] rather than the shared
//! migrated pool because the scheduler owns a dedicated `PgListener`
//! connection and would leak LISTEN state across concurrent tests on a
//! shared DB.

mod common;

use meshmon_service::campaign::dispatch::{
    DirectSettleDispatcher, DispatchOutcome, PairDispatcher,
};
use meshmon_service::campaign::model::{CampaignState, PairResolutionState, ProbeProtocol};
use meshmon_service::campaign::repo::{self, CreateInput};
use meshmon_service::campaign::scheduler::Scheduler;
use meshmon_service::registry::AgentRegistry;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Seed one or more agent rows directly. The scheduler reads through the
/// `AgentRegistry`, which reads from this table — so inserting rows +
/// calling `registry.initial_load()` is how tests populate the active
/// agent pool without standing up the agent registration RPC path.
async fn seed_agents(pool: &sqlx::PgPool, ids: &[&str]) {
    for (i, id) in ids.iter().enumerate() {
        let ip = sqlx::types::ipnetwork::IpNetwork::from(
            IpAddr::from_str(&format!("10.0.0.{}", i + 1)).unwrap(),
        );
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, last_seen_at) \
             VALUES ($1, $2, $3, 3555, 3552, now())",
        )
        .bind(id)
        .bind(format!("Test Agent {i}"))
        .bind(ip)
        .execute(pool)
        .await
        .unwrap();
    }
}

fn create_input(title: &str, agent: &str, destinations: Vec<IpAddr>) -> CreateInput {
    CreateInput {
        title: title.into(),
        notes: String::new(),
        protocol: ProbeProtocol::Icmp,
        source_agent_ids: vec![agent.into()],
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
    }
}

#[tokio::test]
async fn scheduler_dispatches_pairs_for_running_campaign() {
    let db = common::own_container().await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_agents(&db.pool, &["agent-a"]).await;

    let row = repo::create(
        &db.pool,
        create_input(
            "sched-basic",
            "agent-a",
            vec![IpAddr::from_str("198.51.100.50").unwrap()],
        ),
    )
    .await
    .unwrap();
    repo::start(&db.pool, row.id).await.unwrap();

    let registry = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.unwrap();

    let dispatcher = Arc::new(DirectSettleDispatcher {
        pool: db.pool.clone(),
        settle_to: PairResolutionState::Succeeded,
    });

    let scheduler = Scheduler::new(
        db.pool.clone(),
        registry,
        dispatcher,
        /*tick_ms=*/ 100,
        /*chunk_size=*/ 32,
        /*per_destination_rps=*/ 10,
        /*max_pair_attempts=*/ 3,
        /*target_active_window=*/ Duration::from_secs(300),
    );
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(scheduler.run(cancel.clone()));

    // Poll up to ~6 s for the pair to settle.
    let mut settled = false;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let state: Option<PairResolutionState> = sqlx::query_scalar(
            "SELECT resolution_state FROM campaign_pairs WHERE campaign_id = $1 LIMIT 1",
        )
        .bind(row.id)
        .fetch_optional(&db.pool)
        .await
        .unwrap();
        if state == Some(PairResolutionState::Succeeded) {
            settled = true;
            break;
        }
    }
    assert!(settled, "pair did not settle within 6 s");

    // Campaign auto-completes once all pairs are terminal.
    let mut completed = false;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let state: CampaignState =
            sqlx::query_scalar("SELECT state FROM measurement_campaigns WHERE id = $1")
                .bind(row.id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        if state == CampaignState::Completed {
            completed = true;
            break;
        }
    }
    assert!(completed, "campaign did not auto-complete");

    cancel.cancel();
    handle.await.unwrap();
    db.close().await;
}

#[tokio::test]
async fn scheduler_fair_rr_interleaves_two_campaigns() {
    let db = common::own_container().await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_agents(&db.pool, &["agent-fair"]).await;

    // Two campaigns on the same agent, each with 500 pairs. The fair-RR
    // cursor should interleave batches between them so that by the time
    // one finishes, the other is at least 90% done. Destinations are
    // drawn from a /23 sub-allocation so every pair is unique (the
    // `(campaign, source, destination)` uniqueness constraint on
    // `campaign_pairs` otherwise collapses duplicates on insert).
    let destinations: Vec<IpAddr> = (0..500)
        .map(|i| {
            let third = 100 + (i / 256) as u8;
            let fourth = (i % 256) as u8;
            IpAddr::from_str(&format!("198.51.{third}.{fourth}")).unwrap()
        })
        .collect();

    let c1 = repo::create(
        &db.pool,
        create_input("fair-a", "agent-fair", destinations.clone()),
    )
    .await
    .unwrap();
    let c2 = repo::create(
        &db.pool,
        create_input("fair-b", "agent-fair", destinations.clone()),
    )
    .await
    .unwrap();
    repo::start(&db.pool, c1.id).await.unwrap();
    repo::start(&db.pool, c2.id).await.unwrap();

    let registry = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.unwrap();
    let dispatcher = Arc::new(DirectSettleDispatcher {
        pool: db.pool.clone(),
        settle_to: PairResolutionState::Succeeded,
    });

    // High rps so the per-destination bucket never blocks the fairness
    // assertion — this test targets the RR interleaving, not rate-limiting.
    let scheduler = Scheduler::new(
        db.pool.clone(),
        registry,
        dispatcher,
        /*tick_ms=*/ 50,
        /*chunk_size=*/ 32,
        /*per_destination_rps=*/ 1000,
        /*max_pair_attempts=*/ 3,
        Duration::from_secs(300),
    );
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(scheduler.run(cancel.clone()));

    // Wait up to 30 s for one campaign to fully settle. Per-destination
    // bucket capped at 1000 rps means 500 settlements fit comfortably.
    let done_states = &["succeeded", "reused", "unreachable", "skipped"];
    let done_sql = "SELECT COUNT(*) FROM campaign_pairs \
                    WHERE campaign_id = $1 \
                      AND resolution_state::text = ANY($2)";

    let mut done_c1: i64 = 0;
    let mut done_c2: i64 = 0;
    let mut converged = false;
    for _ in 0..300 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        done_c1 = sqlx::query_scalar(done_sql)
            .bind(c1.id)
            .bind(done_states)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        done_c2 = sqlx::query_scalar(done_sql)
            .bind(c2.id)
            .bind(done_states)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if done_c1 == 500 || done_c2 == 500 {
            converged = true;
            break;
        }
    }
    assert!(
        converged,
        "scheduler did not finish one campaign within 30s (c1 done = {done_c1}, c2 done = {done_c2})"
    );

    // At the moment the first campaign finishes, the other must be at
    // least 90% done — this is the fair-RR contract (batches interleave
    // within 10%).
    let (leader_done, follower_done) = if done_c1 == 500 {
        (done_c1, done_c2)
    } else {
        (done_c2, done_c1)
    };
    assert_eq!(leader_done, 500, "sanity: leader must be fully done");
    assert!(
        follower_done as f64 / 500.0 >= 0.90,
        "fair RR violated: when one campaign finished, the other should be >=90% done; \
         follower_done = {follower_done}",
    );

    cancel.cancel();
    handle.await.unwrap();
    db.close().await;
}

#[tokio::test]
async fn stop_prevents_further_dispatch() {
    let db = common::own_container().await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_agents(&db.pool, &["agent-stop"]).await;

    let row = repo::create(
        &db.pool,
        create_input(
            "sched-stop",
            "agent-stop",
            (1..=20)
                .map(|i| IpAddr::from_str(&format!("198.51.100.{i}")).unwrap())
                .collect(),
        ),
    )
    .await
    .unwrap();
    repo::start(&db.pool, row.id).await.unwrap();

    let registry = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.unwrap();

    // Slow dispatcher so `stop` lands mid-run. Each batch takes 300 ms so
    // the ~200 ms warm-up + stop happens while at most one batch has been
    // flushed.
    struct SlowDispatch {
        pool: sqlx::PgPool,
        delay: Duration,
    }
    #[async_trait::async_trait]
    impl PairDispatcher for SlowDispatch {
        async fn dispatch(
            &self,
            _agent: &str,
            batch: Vec<meshmon_service::campaign::dispatch::PendingPair>,
        ) -> DispatchOutcome {
            tokio::time::sleep(self.delay).await;
            for p in &batch {
                sqlx::query!(
                    "UPDATE campaign_pairs SET resolution_state='succeeded', settled_at=now() \
                     WHERE id = $1",
                    p.pair_id
                )
                .execute(&self.pool)
                .await
                .unwrap();
            }
            DispatchOutcome {
                dispatched: batch.len(),
                ..Default::default()
            }
        }
    }

    let dispatcher = Arc::new(SlowDispatch {
        pool: db.pool.clone(),
        delay: Duration::from_millis(300),
    });
    let scheduler = Scheduler::new(
        db.pool.clone(),
        registry,
        dispatcher,
        /*tick_ms=*/ 50,
        /*chunk_size=*/ 4,
        /*per_destination_rps=*/ 1000,
        /*max_pair_attempts=*/ 3,
        Duration::from_secs(300),
    );
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(scheduler.run(cancel.clone()));

    // Let the scheduler pick up at least one batch before stopping.
    tokio::time::sleep(Duration::from_millis(200)).await;
    repo::stop(&db.pool, row.id).await.unwrap();

    // Wait for any in-flight dispatched pairs to drain.
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM campaign_pairs \
              WHERE campaign_id = $1 \
                AND resolution_state::text IN ('pending','dispatched')",
        )
        .bind(row.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        if pending == 0 {
            break;
        }
    }

    // `stop` flips every pending pair to `skipped`; with 20 pairs and a
    // 300 ms batch size of 4, at least some must have been left in
    // pending when stop ran.
    let skipped: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
          WHERE campaign_id = $1 \
            AND resolution_state = 'skipped'",
    )
    .bind(row.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        skipped >= 1,
        "stop must turn at least some pending pairs into skipped (got {skipped})"
    );

    let state: CampaignState =
        sqlx::query_scalar("SELECT state FROM measurement_campaigns WHERE id = $1")
            .bind(row.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(state, CampaignState::Stopped);

    cancel.cancel();
    handle.await.unwrap();
    db.close().await;
}
