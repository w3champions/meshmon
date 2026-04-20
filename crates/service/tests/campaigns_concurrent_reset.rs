//! Concurrent-reset race test.
//!
//! Proves the `resolution_state = 'dispatched'` gate on `SettleWriter`
//! holds against an operator-initiated `apply_edit{force_measurement=true}`
//! that lands between batch claim and settle. The writer UPDATE returns
//! 0 rows and the late settle is a no-op; the pair stays in whatever
//! reset state the edit transitioned it into.

#[path = "common/mod.rs"]
mod common;

use async_trait::async_trait;
use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{MeasurementResult, MeasurementSummary};
use meshmon_service::campaign::dispatch::{DispatchOutcome, PairDispatcher, PendingPair};
use meshmon_service::campaign::model::ProbeProtocol;
use meshmon_service::campaign::repo::{self, CreateInput, EditInput};
use meshmon_service::campaign::scheduler::Scheduler;
use meshmon_service::campaign::writer::{SettleOutcome, SettleWriter};
use meshmon_service::registry::AgentRegistry;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Test-unique destination so parallel runs cannot collide on the
/// `(campaign, source, dest)` uniqueness constraint. Allocations stay
/// inside TEST-NET-2 (RFC 5737) to make intent obvious.
fn unique_dest() -> IpAddr {
    static COUNTER: AtomicU32 = AtomicU32::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    IpAddr::from([198, 51, 100, (n % 254 + 1) as u8])
}

#[tokio::test]
async fn late_settle_is_dropped_when_pair_was_reset() {
    let pool = common::shared_migrated_pool().await;
    let dest = unique_dest();
    let agent_id = "agent-concurrent-reset";

    // Seed a minimal agent row — the registry is not involved here; we
    // only exercise the writer's predicate against a real row.
    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, last_seen_at) \
         VALUES ($1, $1, '198.51.100.1'::inet, 7000, 7001, now()) \
         ON CONFLICT (id) DO UPDATE SET last_seen_at = now()",
    )
    .bind(agent_id)
    .execute(&pool)
    .await
    .expect("seed agent");

    let campaign = repo::create(
        &pool,
        CreateInput {
            title: "concurrent-reset".into(),
            notes: "".into(),
            protocol: ProbeProtocol::Icmp,
            source_agent_ids: vec![agent_id.to_string()],
            destination_ips: vec![dest],
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
    repo::start(&pool, campaign.id).await.expect("start");

    // Claim the pair — this is the state the dispatcher would see
    // right after `take_pending_batch` returns. Row is now `dispatched`
    // with attempt_count = 1.
    let batch = repo::take_pending_batch(&pool, campaign.id, agent_id, 10)
        .await
        .expect("take batch");
    assert_eq!(batch.len(), 1, "expected one pair seeded");
    let pair_id = batch[0].id;

    // Flip the campaign to `stopped` so `apply_edit` is allowed (the
    // repo rejects edits against running campaigns to avoid concurrent
    // state machine racing with the scheduler).
    sqlx::query("UPDATE measurement_campaigns SET state = 'stopped' WHERE id = $1")
        .bind(campaign.id)
        .execute(&pool)
        .await
        .expect("stop campaign");

    // `force_measurement = true` resets every non-delta pair back to
    // pending — including our in-flight dispatched one. This models an
    // operator-driven mid-flight reset.
    repo::apply_edit(
        &pool,
        campaign.id,
        EditInput {
            add_pairs: vec![],
            remove_pairs: vec![],
            force_measurement: Some(true),
        },
    )
    .await
    .expect("apply_edit");

    let state: String =
        sqlx::query_scalar("SELECT resolution_state::text FROM campaign_pairs WHERE id = $1")
            .bind(pair_id)
            .fetch_one(&pool)
            .await
            .expect("read state");
    assert_eq!(state, "pending", "reset must flip dispatched → pending");

    // A late settle arrives — the writer's predicate forces a no-op.
    let writer = SettleWriter::new(pool.clone());
    let pair = PendingPair {
        pair_id,
        campaign_id: campaign.id,
        source_agent_id: agent_id.to_string(),
        destination_ip: IpAddr::from_str(&dest.to_string()).expect("ip roundtrip"),
        probe_count: 10,
        timeout_ms: 2_000,
        probe_stagger_ms: 100,
        force_measurement: true,
        protocol: ProbeProtocol::Icmp,
    };
    let result = MeasurementResult {
        pair_id: pair_id as u64,
        outcome: Some(Outcome::Success(MeasurementSummary {
            attempted: 10,
            succeeded: 10,
            latency_min_ms: 1.0,
            latency_avg_ms: 1.0,
            latency_median_ms: 1.0,
            latency_p95_ms: 1.0,
            latency_max_ms: 1.0,
            latency_stddev_ms: 0.0,
            loss_pct: 0.0,
        })),
    };
    let settled = writer.settle(&pair, &result).await.expect("settle");
    assert_eq!(
        settled,
        SettleOutcome::RaceLost,
        "late settle must be a no-op when pair was reset",
    );

    // The pair is still pending; the reset survived the race.
    let state: String =
        sqlx::query_scalar("SELECT resolution_state::text FROM campaign_pairs WHERE id = $1")
            .bind(pair_id)
            .fetch_one(&pool)
            .await
            .expect("read state");
    assert_eq!(
        state, "pending",
        "concurrent reset must be preserved by the writer's gate"
    );
    let measurement_id: Option<i64> =
        sqlx::query_scalar("SELECT measurement_id FROM campaign_pairs WHERE id = $1")
            .bind(pair_id)
            .fetch_one(&pool)
            .await
            .expect("read measurement_id");
    assert!(
        measurement_id.is_none(),
        "pair measurement_id must stay NULL after reset-then-late-settle",
    );

    repo::delete(&pool, campaign.id).await.expect("cleanup");
    sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(agent_id)
        .execute(&pool)
        .await
        .expect("cleanup agent");
}

/// Custom dispatcher that parks the batch on a shared `Mutex` so the
/// test can inject an operator reset between the dispatcher returning
/// and the scheduler running its revert UPDATE. Returns every pair as
/// `rejected_ids` so we hit the scheduler's dispatcher-rejection
/// revert path — which must be gated on `resolution_state='dispatched'`
/// to survive the concurrent reset.
struct RejectAfterBlockDispatcher {
    released: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    observed: Arc<Mutex<Vec<i64>>>,
}

#[async_trait]
impl PairDispatcher for RejectAfterBlockDispatcher {
    async fn dispatch(&self, _agent: &str, batch: Vec<PendingPair>) -> DispatchOutcome {
        let ids: Vec<i64> = batch.iter().map(|p| p.pair_id).collect();
        *self.observed.lock().await = ids.clone();
        if let Some(rx) = self.released.lock().await.take() {
            let _ = rx.await;
        }
        DispatchOutcome {
            dispatched: 0,
            rejected_ids: ids,
            rate_limited_ids: Vec::new(),
            skipped_reason: Some("test-reject".into()),
        }
    }
}

#[tokio::test]
async fn scheduler_revert_does_not_clobber_concurrent_reset() {
    let pool = common::shared_migrated_pool().await;
    let dest = unique_dest();
    let agent_id = "agent-scheduler-revert-race";

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, last_seen_at) \
         VALUES ($1, $1, '198.51.100.2'::inet, 7000, 7001, now()) \
         ON CONFLICT (id) DO UPDATE SET last_seen_at = now()",
    )
    .bind(agent_id)
    .execute(&pool)
    .await
    .expect("seed agent");

    let campaign = repo::create(
        &pool,
        CreateInput {
            title: "scheduler-revert-race".into(),
            notes: "".into(),
            protocol: ProbeProtocol::Icmp,
            source_agent_ids: vec![agent_id.to_string()],
            destination_ips: vec![dest],
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
    repo::start(&pool, campaign.id).await.expect("start");

    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let observed = Arc::new(Mutex::new(Vec::<i64>::new()));
    let dispatcher = Arc::new(RejectAfterBlockDispatcher {
        released: Arc::new(Mutex::new(Some(release_rx))),
        observed: observed.clone(),
    });

    let registry = Arc::new(AgentRegistry::new(
        pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.expect("registry load");

    let scheduler = Scheduler::new(
        pool.clone(),
        registry,
        dispatcher,
        /*tick_ms=*/ 50,
        /*chunk_size=*/ 10,
        /*max_pair_attempts=*/ 3,
        /*target_active_window=*/ Duration::from_secs(300),
    );
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(scheduler.run(cancel.clone()));

    // Wait for the scheduler to claim the pair and hand it to the
    // dispatcher (which then parks on `released`).
    let pair_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let guard = observed.lock().await;
            if let Some(id) = guard.first() {
                break *id;
            }
            drop(guard);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("dispatcher saw the batch within 5 s");

    // Pair is `dispatched`; dispatcher is parked. Simulate an operator
    // action racing the dispatcher — flip the row to `skipped` with a
    // distinctive `last_error` tag so the assertion below fires iff the
    // scheduler's revert UPDATE clobbers the reset back to `pending`.
    sqlx::query(
        "UPDATE campaign_pairs \
            SET resolution_state = 'skipped', last_error = 'operator-reset-marker' \
          WHERE id = $1",
    )
    .bind(pair_id)
    .execute(&pool)
    .await
    .expect("mark pair skipped");

    // Release the dispatcher; its DispatchOutcome returns `rejected_ids`
    // so the scheduler runs the revert UPDATE.
    release_tx.send(()).expect("release dispatcher");

    // Poll for the race to resolve. The revert must NOT clobber the
    // `skipped` state — the gate on `resolution_state = 'dispatched'`
    // makes the UPDATE a no-op for the now-non-dispatched row.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut final_state = String::new();
    while std::time::Instant::now() < deadline {
        final_state = sqlx::query_scalar::<_, String>(
            "SELECT resolution_state::text FROM campaign_pairs WHERE id = $1",
        )
        .bind(pair_id)
        .fetch_one(&pool)
        .await
        .expect("read state");
        if final_state != "dispatched" {
            // Either the race has fully played out to `skipped` (good)
            // or it clobbered to `pending` (bad) — break and assert.
            tokio::time::sleep(Duration::from_millis(150)).await;
            final_state = sqlx::query_scalar::<_, String>(
                "SELECT resolution_state::text FROM campaign_pairs WHERE id = $1",
            )
            .bind(pair_id)
            .fetch_one(&pool)
            .await
            .expect("read state");
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(
        final_state, "skipped",
        "scheduler revert must not clobber the concurrent operator reset",
    );

    cancel.cancel();
    let _ = handle.await;
    repo::delete(&pool, campaign.id).await.expect("cleanup");
    sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(agent_id)
        .execute(&pool)
        .await
        .expect("cleanup agent");
}
