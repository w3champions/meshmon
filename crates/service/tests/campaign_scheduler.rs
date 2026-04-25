//! Scheduler integration tests.
//!
//! Every test here uses [`common::own_container`] rather than the shared
//! migrated pool because the scheduler owns a dedicated `PgListener`
//! connection and would leak LISTEN state across concurrent tests on a
//! shared DB.

mod common;

use meshmon_service::campaign::dispatch::{
    DirectSettleDispatcher, DispatchOutcome, PairDispatcher, PendingPair, RendezvousDispatcher,
};
use meshmon_service::campaign::model::{
    CampaignState, MeasurementKind, PairResolutionState, ProbeProtocol,
};
use meshmon_service::campaign::repo::{self, CreateInput};
use meshmon_service::campaign::scheduler::Scheduler;
use meshmon_service::registry::AgentRegistry;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
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
             VALUES ($1, $2, $3, 8002, 8005, now())",
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
        loss_threshold_ratio: None,
        stddev_weight: None,
        evaluation_mode: None,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
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

    // Per-destination rate limiting lives on the dispatcher; this
    // scheduler test stubs dispatch so no bucket interferes with the RR
    // fairness assertion.
    let scheduler = Scheduler::new(
        db.pool.clone(),
        registry,
        dispatcher,
        /*tick_ms=*/ 50,
        /*chunk_size=*/ 32,
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

#[tokio::test]
async fn scheduler_reverts_rejected_pairs_to_pending() {
    // A dispatcher that rejects every pair it receives. The scheduler
    // must flip those pairs from `dispatched` back to `pending` so
    // `expire_stale_attempts` can eventually skip them via
    // `max_pair_attempts`. Without the revert, the campaign stays in
    // `running` forever (codex iter-6 finding).
    let db = common::own_container().await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_agents(&db.pool, &["agent-reject"]).await;

    let row = repo::create(
        &db.pool,
        create_input(
            "sched-reject",
            "agent-reject",
            vec![IpAddr::from_str("198.51.100.60").unwrap()],
        ),
    )
    .await
    .unwrap();
    repo::start(&db.pool, row.id).await.unwrap();

    struct RejectAll;
    #[async_trait::async_trait]
    impl PairDispatcher for RejectAll {
        async fn dispatch(
            &self,
            _agent: &str,
            batch: Vec<meshmon_service::campaign::dispatch::PendingPair>,
        ) -> DispatchOutcome {
            DispatchOutcome {
                dispatched: 0,
                rejected_ids: batch.iter().map(|p| p.pair_id).collect(),
                rate_limited_ids: Vec::new(),
                skipped_reason: Some("test-rejects-all".into()),
            }
        }
    }

    let registry = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.unwrap();

    let scheduler = Scheduler::new(
        db.pool.clone(),
        registry,
        Arc::new(RejectAll),
        /*tick_ms=*/ 80,
        /*chunk_size=*/ 32,
        /*max_pair_attempts=*/ 3,
        /*target_active_window=*/ Duration::from_secs(300),
    );
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(scheduler.run(cancel.clone()));

    // Poll up to 6 s. With 3 attempt budget and 80 ms tick, the pair
    // gets claimed → rejected → pending three times, then skipped.
    let mut final_state = None;
    for _ in 0..120 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let row: (PairResolutionState, i16) = sqlx::query_as(
            "SELECT resolution_state, attempt_count FROM campaign_pairs \
              WHERE campaign_id = $1 LIMIT 1",
        )
        .bind(row.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        if row.0 == PairResolutionState::Skipped {
            final_state = Some(row);
            break;
        }
    }
    let (state, attempts) = final_state.expect("pair did not reach skipped within 6 s");
    assert_eq!(state, PairResolutionState::Skipped);
    assert!(
        attempts >= 3,
        "attempt_count must reach the max threshold; got {attempts}"
    );

    cancel.cancel();
    handle.await.unwrap();
    db.close().await;
}

/// Records every `(agent_id, kind, probe_count, pair_ids)` batch the
/// scheduler hands in, and settles each pair synchronously so the tick
/// loop makes progress without dispatcher retries polluting the record.
#[derive(Default, Clone)]
struct RecordingDispatcher {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    pool: Option<sqlx::PgPool>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct RecordedCall {
    agent_id: String,
    kind: MeasurementKind,
    probe_count: i16,
    pair_ids: Vec<i64>,
}

#[async_trait::async_trait]
impl PairDispatcher for RecordingDispatcher {
    async fn dispatch(&self, agent_id: &str, batch: Vec<PendingPair>) -> DispatchOutcome {
        let head = batch[0].clone();
        let pair_ids: Vec<i64> = batch.iter().map(|p| p.pair_id).collect();
        self.calls.lock().await.push(RecordedCall {
            agent_id: agent_id.to_string(),
            kind: head.kind,
            probe_count: head.probe_count,
            pair_ids: pair_ids.clone(),
        });
        if let Some(pool) = &self.pool {
            for p in &batch {
                sqlx::query!(
                    "UPDATE campaign_pairs \
                        SET resolution_state='succeeded', settled_at=now() \
                      WHERE id=$1",
                    p.pair_id
                )
                .execute(pool)
                .await
                .expect("record dispatcher settle");
            }
        }
        DispatchOutcome {
            dispatched: batch.len(),
            ..Default::default()
        }
    }
}

/// Guards the P1 fix: heterogeneous `campaign_pairs.kind` in a single
/// `(campaign, agent)` claim must be split into one dispatch call per
/// kind, each carrying the kind-specific `probe_count`.
#[tokio::test]
async fn scheduler_splits_batches_by_kind_with_kind_specific_probe_count() {
    let db = common::own_container().await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_agents(&db.pool, &["agent-kind"]).await;

    // Baseline campaign with a distinct detail probe count so the test
    // can verify the override plumbs through end-to-end.
    let input = CreateInput {
        title: "kind-split".into(),
        notes: String::new(),
        protocol: ProbeProtocol::Icmp,
        source_agent_ids: vec!["agent-kind".into()],
        destination_ips: vec![IpAddr::from_str("198.51.100.77").unwrap()],
        force_measurement: true, // skip reuse so the baseline pair dispatches every time
        probe_count: Some(12),
        probe_count_detail: Some(240),
        timeout_ms: None,
        probe_stagger_ms: None,
        loss_threshold_ratio: None,
        stddev_weight: None,
        evaluation_mode: None,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
        created_by: None,
    };
    let row = repo::create(&db.pool, input).await.unwrap();
    repo::start(&db.pool, row.id).await.unwrap();

    // Inject one extra detail_mtr pair against the same (source, destination)
    // so the claim batch is heterogeneous in `kind`.
    sqlx::query(
        "INSERT INTO campaign_pairs \
            (campaign_id, source_agent_id, destination_ip, resolution_state, kind) \
          VALUES ($1, $2, $3, 'pending', 'detail_mtr'::measurement_kind)",
    )
    .bind(row.id)
    .bind("agent-kind")
    .bind(sqlx::types::ipnetwork::IpNetwork::from(
        IpAddr::from_str("198.51.100.77").unwrap(),
    ))
    .execute(&db.pool)
    .await
    .unwrap();

    let registry = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.unwrap();

    let dispatcher = Arc::new(RecordingDispatcher {
        calls: Arc::new(Mutex::new(Vec::new())),
        pool: Some(db.pool.clone()),
    });
    let scheduler = Scheduler::new(
        db.pool.clone(),
        registry,
        dispatcher.clone(),
        /*tick_ms=*/ 100,
        /*chunk_size=*/ 32,
        /*max_pair_attempts=*/ 3,
        /*target_active_window=*/ Duration::from_secs(300),
    );
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(scheduler.run(cancel.clone()));

    // Poll until we see at least two dispatch calls (one per kind) with
    // the pair settled for the campaign.
    let mut saw_campaign = None;
    let mut saw_detail_mtr = None;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let calls = dispatcher.calls.lock().await.clone();
        for c in calls {
            match c.kind {
                MeasurementKind::Campaign if saw_campaign.is_none() => saw_campaign = Some(c),
                MeasurementKind::DetailMtr if saw_detail_mtr.is_none() => saw_detail_mtr = Some(c),
                _ => {}
            }
        }
        if saw_campaign.is_some() && saw_detail_mtr.is_some() {
            break;
        }
    }
    cancel.cancel();
    handle.await.unwrap();

    let camp_call = saw_campaign.expect("scheduler never dispatched the campaign baseline pair");
    let mtr_call = saw_detail_mtr.expect("scheduler never dispatched the detail_mtr pair");

    assert_eq!(camp_call.kind, MeasurementKind::Campaign);
    assert_eq!(
        camp_call.probe_count, 12,
        "campaign batch probe_count must match campaigns.probe_count"
    );
    assert_eq!(
        camp_call.pair_ids.len(),
        1,
        "campaign kind batch must carry exactly the baseline pair"
    );

    assert_eq!(mtr_call.kind, MeasurementKind::DetailMtr);
    assert_eq!(
        mtr_call.probe_count, 1,
        "detail_mtr batch probe_count must be 1 per MTR spec"
    );
    assert_eq!(
        mtr_call.pair_ids.len(),
        1,
        "detail_mtr kind batch must carry exactly the detail pair"
    );
    assert_ne!(
        camp_call.pair_ids, mtr_call.pair_ids,
        "kind split must produce disjoint dispatch calls"
    );

    db.close().await;
}

/// Fan-out contract: within one tick, the scheduler MUST dispatch
/// multiple agents concurrently — i.e. at some moment, N agents' calls
/// to `PairDispatcher::dispatch` are simultaneously in flight.
///
/// Proof shape: a `RendezvousDispatcher` with `expected_agents = 2`
/// holds every call at a barrier that releases only when both have
/// arrived. A serial scheduler parks the first call forever (the
/// second call never starts), so the whole `tick_once` blocks; the
/// test's `tokio::time::timeout` surfaces that as a RED failure. A
/// fan-out scheduler drives both calls into the barrier, releases,
/// and completes well within the timeout (GREEN).
///
/// This test does not assert wall-clock timing of any kind; it asserts
/// the *behavioural* property that two dispatch futures can be
/// simultaneously in flight. The 5 s timeout is a safety net for the
/// deadlock case, not a timing bound.
#[tokio::test]
async fn fanout_dispatches_agents_concurrently() {
    let db = common::own_container().await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    // Two source agents, each with one pending pair against a unique
    // destination so neither is reuse-eligible.
    seed_agents(&db.pool, &["agent-a", "agent-b"]).await;

    let camp_a = repo::create(
        &db.pool,
        create_input(
            "fanout-a",
            "agent-a",
            vec![IpAddr::from_str("203.0.113.1").unwrap()],
        ),
    )
    .await
    .unwrap();
    repo::start(&db.pool, camp_a.id).await.unwrap();

    let camp_b = repo::create(
        &db.pool,
        create_input(
            "fanout-b",
            "agent-b",
            vec![IpAddr::from_str("203.0.113.2").unwrap()],
        ),
    )
    .await
    .unwrap();
    repo::start(&db.pool, camp_b.id).await.unwrap();

    // Barrier size = number of agents we expect to see in-flight
    // simultaneously.
    let dispatcher = Arc::new(RendezvousDispatcher::new(2));

    let registry = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.unwrap();

    let scheduler = Scheduler::new(
        db.pool.clone(),
        registry,
        dispatcher.clone(),
        /*tick_ms=*/ 100,
        /*chunk_size=*/ 32,
        /*max_pair_attempts=*/ 3,
        /*target_active_window=*/ Duration::from_secs(300),
    );

    // Run a single tick under a timeout. Serial dispatch deadlocks at
    // the barrier; fan-out completes in milliseconds.
    let tick = tokio::time::timeout(Duration::from_secs(5), scheduler.tick_once_for_test()).await;

    match tick {
        Err(_) => panic!(
            "tick_once timed out waiting at the rendezvous barrier — agents \
             were dispatched serially, not concurrently"
        ),
        Ok(Err(e)) => panic!("tick_once returned error: {e:?}"),
        Ok(Ok(())) => {}
    }

    let calls = dispatcher.calls.lock().await;
    assert_eq!(
        calls.len(),
        2,
        "expected 2 dispatch calls, got {}",
        calls.len()
    );
    let agent_ids: std::collections::HashSet<&str> = calls.iter().map(String::as_str).collect();
    assert!(agent_ids.contains("agent-a"));
    assert!(agent_ids.contains("agent-b"));

    db.close().await;
}
