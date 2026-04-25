//! Integration tests for the campaign settle writer.
//!
//! Every test seeds a dispatched campaign pair, invokes
//! `SettleWriter::settle`, and asserts on the resulting DB state:
//!   * `campaign_pairs` row transitioned to the expected terminal
//!     state with the right `last_error` tag,
//!   * `measurements` / `mtr_traces` rows were (or were not) created,
//!   * the `campaign_pair_settled` NOTIFY fired with the campaign id,
//!   * a concurrent reset between claim and settle is preserved.

#[path = "common/mod.rs"]
mod common;

use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{
    HopIp, HopSummary, MeasurementFailure, MeasurementFailureCode, MeasurementResult,
    MeasurementSummary, MtrTraceResult,
};
use meshmon_service::campaign::dispatch::PendingPair;
use meshmon_service::campaign::model::{MeasurementKind, ProbeProtocol};
use meshmon_service::campaign::repo::{self, CreateInput};
use meshmon_service::campaign::writer::{SettleOutcome, SettleWriter};
use sqlx::PgPool;
use std::net::IpAddr;

/// Allocate a test-unique destination IP so concurrent tests against
/// the shared pool cannot cross-pollute `measurements` rows or the
/// `campaign_pairs` UNIQUE constraint on (campaign, source, dest).
/// The prefix 203.0.113.0/24 is TEST-NET-3 per RFC 5737.
fn unique_dest() -> IpAddr {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Keep within the /24 — 254 distinct values are plenty for one
    // test binary, and staying inside TEST-NET-3 makes the intent clear.
    IpAddr::from([203, 0, 113, (n % 254 + 1) as u8])
}

/// Build a fresh campaign with one pair using a unique destination IP,
/// then flip the pair to `dispatched` via `take_pending_batch` so the
/// writer's `resolution_state='dispatched'` gate will match.
async fn seed_dispatched_pair(pool: &PgPool) -> (uuid::Uuid, i64, IpAddr) {
    let dest = unique_dest();
    let campaign = repo::create(
        pool,
        CreateInput {
            title: "writer-test".into(),
            notes: "".into(),
            protocol: ProbeProtocol::Icmp,
            source_agent_ids: vec!["agent-a".into()],
            destination_ips: vec![dest],
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
        },
    )
    .await
    .expect("create campaign");
    repo::start(pool, campaign.id).await.expect("start");
    let batch = repo::take_pending_batch(pool, campaign.id, "agent-a", 10)
        .await
        .expect("take batch");
    assert_eq!(batch.len(), 1, "expected exactly one seeded pair");
    (campaign.id, batch[0].id, dest)
}

fn mk_pair(campaign_id: uuid::Uuid, pair_id: i64, dest: IpAddr) -> PendingPair {
    PendingPair {
        pair_id,
        campaign_id,
        source_agent_id: "agent-a".into(),
        destination_ip: dest,
        probe_count: 10,
        timeout_ms: 2_000,
        probe_stagger_ms: 100,
        force_measurement: false,
        protocol: ProbeProtocol::Icmp,
        kind: MeasurementKind::Campaign,
    }
}

fn ok_result(pair_id: i64) -> MeasurementResult {
    MeasurementResult {
        pair_id: pair_id as u64,
        outcome: Some(Outcome::Success(MeasurementSummary {
            attempted: 10,
            succeeded: 10,
            latency_min_ms: 1.0,
            latency_avg_ms: 1.5,
            latency_median_ms: 1.4,
            latency_p95_ms: 2.0,
            latency_max_ms: 2.5,
            latency_stddev_ms: 0.3,
            loss_ratio: 0.0,
        })),
    }
}

fn failure_result(pair_id: i64, code: MeasurementFailureCode) -> MeasurementResult {
    MeasurementResult {
        pair_id: pair_id as u64,
        outcome: Some(Outcome::Failure(MeasurementFailure {
            code: code as i32,
            detail: format!("{code:?}"),
        })),
    }
}

#[tokio::test]
async fn settle_success_writes_measurement_and_flips_pair_to_succeeded() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    let settled = writer
        .settle(&mk_pair(campaign_id, pair_id, dest), &ok_result(pair_id))
        .await
        .expect("settle");
    assert_eq!(settled, SettleOutcome::Settled);

    let (state, measurement_id, last_error): (String, Option<i64>, Option<String>) =
        sqlx::query_as(
            "SELECT resolution_state::text, measurement_id, last_error \
             FROM campaign_pairs WHERE id = $1",
        )
        .bind(pair_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "succeeded");
    assert!(measurement_id.is_some());
    assert!(last_error.is_none());

    // The inserted measurement must carry the dispatching pair's kind;
    // `mk_pair` uses `MeasurementKind::Campaign`, so the row must be
    // `kind='campaign'`. See `settle_detail_ping_success_writes_detail_kind`
    // for the `/detail`-path assertion.
    let m_id = measurement_id.unwrap();
    let (kind, probe_count, loss_ratio): (String, i16, f32) = sqlx::query_as(
        "SELECT kind::text, probe_count, loss_ratio FROM measurements WHERE id = $1",
    )
    .bind(m_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kind, "campaign");
    assert_eq!(probe_count, 10);
    assert_eq!(loss_ratio, 0.0);

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_detail_ping_success_writes_detail_kind() {
    // A `detail_ping` pair dispatched through the latency RPC path must
    // persist `measurements.kind = 'detail_ping'`, not `'campaign'`.
    // Without this, detail data is indistinguishable from baseline in
    // the measurements table and the evaluator's reuse window can bind
    // baseline pairs to detail measurements with no way to tell.
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    let mut pair = mk_pair(campaign_id, pair_id, dest);
    pair.kind = MeasurementKind::DetailPing;

    let settled = writer.settle(&pair, &ok_result(pair_id)).await.unwrap();
    assert_eq!(settled, SettleOutcome::Settled);

    let m_id: i64 = sqlx::query_scalar(
        "SELECT measurement_id FROM campaign_pairs WHERE id = $1 AND measurement_id IS NOT NULL",
    )
    .bind(pair_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let kind: String = sqlx::query_scalar("SELECT kind::text FROM measurements WHERE id = $1")
        .bind(m_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        kind, "detail_ping",
        "detail_ping pair must persist measurements.kind=detail_ping"
    );

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_failure_timeout_writes_unreachable_with_timeout_tag() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    let settled = writer
        .settle(
            &mk_pair(campaign_id, pair_id, dest),
            &failure_result(pair_id, MeasurementFailureCode::Timeout),
        )
        .await
        .expect("settle");
    assert_eq!(settled, SettleOutcome::Settled);

    let (state, measurement_id, last_error): (String, Option<i64>, Option<String>) =
        sqlx::query_as(
            "SELECT resolution_state::text, measurement_id, last_error \
             FROM campaign_pairs WHERE id = $1",
        )
        .bind(pair_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "unreachable");
    assert_eq!(last_error.as_deref(), Some("timeout"));
    // No measurement row for a failure outcome.
    assert!(measurement_id.is_none());

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_no_route_maps_to_unreachable_state() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    writer
        .settle(
            &mk_pair(campaign_id, pair_id, dest),
            &failure_result(pair_id, MeasurementFailureCode::NoRoute),
        )
        .await
        .expect("settle");

    let (state, last_error): (String, Option<String>) = sqlx::query_as(
        "SELECT resolution_state::text, last_error FROM campaign_pairs WHERE id = $1",
    )
    .bind(pair_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "unreachable");
    assert_eq!(last_error.as_deref(), Some("unreachable"));

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_refused_maps_to_unreachable_with_refused_tag() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    writer
        .settle(
            &mk_pair(campaign_id, pair_id, dest),
            &failure_result(pair_id, MeasurementFailureCode::Refused),
        )
        .await
        .expect("settle");

    let (state, last_error): (String, Option<String>) = sqlx::query_as(
        "SELECT resolution_state::text, last_error FROM campaign_pairs WHERE id = $1",
    )
    .bind(pair_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "unreachable");
    assert_eq!(last_error.as_deref(), Some("refused"));

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_cancelled_maps_to_skipped_with_cancelled_tag() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    writer
        .settle(
            &mk_pair(campaign_id, pair_id, dest),
            &failure_result(pair_id, MeasurementFailureCode::Cancelled),
        )
        .await
        .expect("settle");

    let (state, last_error): (String, Option<String>) = sqlx::query_as(
        "SELECT resolution_state::text, last_error FROM campaign_pairs WHERE id = $1",
    )
    .bind(pair_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "skipped");
    assert_eq!(last_error.as_deref(), Some("cancelled"));

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_agent_error_maps_to_skipped_with_agent_rejected_tag() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    writer
        .settle(
            &mk_pair(campaign_id, pair_id, dest),
            &failure_result(pair_id, MeasurementFailureCode::AgentError),
        )
        .await
        .expect("settle");

    let (state, last_error): (String, Option<String>) = sqlx::query_as(
        "SELECT resolution_state::text, last_error FROM campaign_pairs WHERE id = $1",
    )
    .bind(pair_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "skipped");
    assert_eq!(last_error.as_deref(), Some("agent_rejected"));

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_unspecified_maps_to_skipped_with_agent_rejected_tag() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    writer
        .settle(
            &mk_pair(campaign_id, pair_id, dest),
            &failure_result(pair_id, MeasurementFailureCode::Unspecified),
        )
        .await
        .expect("settle");

    let (state, last_error): (String, Option<String>) = sqlx::query_as(
        "SELECT resolution_state::text, last_error FROM campaign_pairs WHERE id = $1",
    )
    .bind(pair_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "skipped");
    assert_eq!(last_error.as_deref(), Some("agent_rejected"));

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_mtr_writes_trace_and_links_measurement() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    let result = MeasurementResult {
        pair_id: pair_id as u64,
        outcome: Some(Outcome::Mtr(MtrTraceResult {
            hops: vec![HopSummary {
                position: 1,
                observed_ips: vec![HopIp {
                    ip: vec![10, 0, 0, 1].into(),
                    frequency: 1.0,
                }],
                avg_rtt_micros: 500,
                stddev_rtt_micros: 0,
                loss_ratio: 0.0,
            }],
        })),
    };

    // MTR outcomes only ever come from `DetailMtr` pairs (the only
    // kind the RPC dispatcher routes through `MeasurementKind::Mtr`),
    // and the writer now derives `measurements.kind` from the pair's
    // kind. Match that invariant explicitly in the fixture.
    let mut pair = mk_pair(campaign_id, pair_id, dest);
    pair.kind = MeasurementKind::DetailMtr;

    writer.settle(&pair, &result).await.expect("settle");

    let (state, measurement_id): (String, Option<i64>) = sqlx::query_as(
        "SELECT resolution_state::text, measurement_id FROM campaign_pairs WHERE id = $1",
    )
    .bind(pair_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "succeeded");
    let m_id = measurement_id.expect("measurement_id set");

    let (kind, mtr_id): (String, Option<i64>) =
        sqlx::query_as("SELECT kind::text, mtr_id FROM measurements WHERE id = $1")
            .bind(m_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kind, "detail_mtr");
    let mtr_id = mtr_id.expect("mtr_id set");

    // Assert the stored hops round-trip through JSONB correctly.
    let hops_json: serde_json::Value =
        sqlx::query_scalar("SELECT hops FROM mtr_traces WHERE id = $1")
            .bind(mtr_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let hops_arr = hops_json.as_array().expect("hops is json array");
    assert_eq!(hops_arr.len(), 1);
    assert_eq!(hops_arr[0]["position"], 1);
    assert_eq!(hops_arr[0]["observed_ips"][0]["ip"], "10.0.0.1");

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_returns_race_lost_when_pair_was_reset_between_claim_and_settle() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    // Simulate a concurrent operator reset: flip the pair back to
    // `pending`. The writer's `AND resolution_state = 'dispatched'`
    // gate must now refuse the update.
    sqlx::query(
        "UPDATE campaign_pairs \
           SET resolution_state = 'pending', dispatched_at = NULL \
         WHERE id = $1",
    )
    .bind(pair_id)
    .execute(&pool)
    .await
    .unwrap();

    let settled = writer
        .settle(&mk_pair(campaign_id, pair_id, dest), &ok_result(pair_id))
        .await
        .expect("settle");
    assert_eq!(
        settled,
        SettleOutcome::RaceLost,
        "late settle against reset pair must be a no-op",
    );

    let state: String =
        sqlx::query_scalar("SELECT resolution_state::text FROM campaign_pairs WHERE id = $1")
            .bind(pair_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "pending", "concurrent reset must be preserved");

    // And because the whole tx rolled back, no measurement row was
    // inserted either — otherwise the agent's work would leak into
    // `measurements` without an attributing pair. Scope the lookup by
    // destination so parallel tests against the shared pool do not
    // cross-pollute the assertion.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM measurements WHERE source_agent_id = $1 AND destination_ip = $2",
    )
    .bind("agent-a")
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dest))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0, "rolled-back tx must not leave a measurement row");

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_emits_campaign_pair_settled_notify() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    // Subscribe *before* firing the settle so the NOTIFY cannot be
    // missed by a race between listener setup and tx commit.
    let mut listener = sqlx::postgres::PgListener::connect_with(&pool)
        .await
        .expect("connect listener");
    listener
        .listen("campaign_pair_settled")
        .await
        .expect("listen");

    let pair = mk_pair(campaign_id, pair_id, dest);
    let result = ok_result(pair_id);

    let writer_task = tokio::spawn(async move { writer.settle(&pair, &result).await });

    // Drain notifications until we find ours — the shared test pool
    // may interleave other writers' notifications on the same channel,
    // so matching purely on the next recv is racy. A 2 s bound keeps
    // the assertion strict enough that a broken NOTIFY still fails
    // quickly.
    let expected = campaign_id.to_string();
    let found = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let notify = listener.recv().await.expect("PgListener recv");
            assert_eq!(notify.channel(), "campaign_pair_settled");
            if notify.payload() == expected {
                break;
            }
        }
    })
    .await;
    found.expect("NOTIFY arrived in time");

    writer_task.await.unwrap().unwrap();

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_success_persists_agent_loss_ratio() {
    // Regression barrier: the whole stack — agent wire, DB, evaluator,
    // DTOs — speaks fraction (0.0–1.0). The writer must store the
    // agent's `loss_ratio` unchanged in `measurements.loss_ratio`; the
    // frontend multiplies by 100 at display time.
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    let result = MeasurementResult {
        pair_id: pair_id as u64,
        outcome: Some(Outcome::Success(MeasurementSummary {
            attempted: 10,
            succeeded: 2,
            latency_min_ms: 1.0,
            latency_avg_ms: 1.5,
            latency_median_ms: 1.4,
            latency_p95_ms: 2.0,
            latency_max_ms: 2.5,
            latency_stddev_ms: 0.3,
            // Agent-wire fraction: 75 % packet loss.
            loss_ratio: 0.75,
        })),
    };

    let settled = writer
        .settle(&mk_pair(campaign_id, pair_id, dest), &result)
        .await
        .expect("settle");
    assert_eq!(settled, SettleOutcome::Settled);

    let m_id: i64 = sqlx::query_scalar(
        "SELECT measurement_id FROM campaign_pairs WHERE id = $1 AND measurement_id IS NOT NULL",
    )
    .bind(pair_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let stored_loss: f32 = sqlx::query_scalar("SELECT loss_ratio FROM measurements WHERE id = $1")
        .bind(m_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        (stored_loss - 0.75f32).abs() < 0.001,
        "expected 0.75 (fraction) stored for a 0.75 wire fraction, got {stored_loss}",
    );

    repo::delete(&pool, campaign_id).await.unwrap();
}

#[tokio::test]
async fn settle_empty_outcome_returns_malformed_and_rolls_back() {
    let pool = common::shared_migrated_pool().await.clone();
    let (campaign_id, pair_id, dest) = seed_dispatched_pair(&pool).await;
    let writer = SettleWriter::new(pool.clone());

    let result = MeasurementResult {
        pair_id: pair_id as u64,
        outcome: None,
    };
    let settled = writer
        .settle(&mk_pair(campaign_id, pair_id, dest), &result)
        .await
        .expect("settle");
    assert_eq!(
        settled,
        SettleOutcome::MalformedNoOutcome,
        "empty outcome is a protocol violation; caller must revert the pair",
    );

    let state: String =
        sqlx::query_scalar("SELECT resolution_state::text FROM campaign_pairs WHERE id = $1")
            .bind(pair_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "dispatched", "empty outcome must not touch state");

    repo::delete(&pool, campaign_id).await.unwrap();
}
