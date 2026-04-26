//! T56 — migration up/down round-trip.

use sqlx::Executor;
mod common;
use common::acquire;

#[tokio::test]
async fn campaigns_edge_candidate_migration_up_down_round_trips() {
    let db = acquire(/*with_timescale=*/ true).await;
    meshmon_service::db::run_migrations(&db.pool)
        .await
        .expect("migrations apply");

    // The shared harness runs UP automatically; verify the schema additions.
    let useful_latency_ms_present: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
            WHERE table_name = 'measurement_campaigns'
              AND column_name = 'useful_latency_ms')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(useful_latency_ms_present, "measurement_campaigns.useful_latency_ms missing after up");

    let max_hops_present: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
            WHERE table_name = 'measurement_campaigns'
              AND column_name = 'max_hops')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(max_hops_present);

    let vm_lookback_present: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
            WHERE table_name = 'measurement_campaigns'
              AND column_name = 'vm_lookback_minutes')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(vm_lookback_present);

    let edge_pair_table_present: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables
            WHERE table_name = 'campaign_evaluation_edge_pair_details')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(edge_pair_table_present);

    let edge_candidate_variant: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_enum
            WHERE enumtypid = 'evaluation_mode'::regtype
              AND enumlabel = 'edge_candidate')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(edge_candidate_variant);

    let candidates_coverage_count: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
            WHERE table_name = 'campaign_evaluation_candidates'
              AND column_name = 'coverage_count')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(candidates_coverage_count);

    let pair_details_substituted: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
            WHERE table_name = 'campaign_evaluation_pair_details'
              AND column_name = 'direct_was_substituted')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(pair_details_substituted);

    let pair_details_x_pos: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
            WHERE table_name = 'campaign_evaluation_pair_details'
              AND column_name = 'winning_x_position')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(pair_details_x_pos);

    // Apply DOWN, then re-verify the schema is reversed.
    db.pool
        .execute(include_str!("../migrations/20260426000000_campaigns_edge_candidate.down.sql"))
        .await
        .expect("down migration should apply");

    let useful_latency_ms_gone: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM information_schema.columns
            WHERE table_name = 'measurement_campaigns'
              AND column_name = 'useful_latency_ms')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(useful_latency_ms_gone);

    let edge_pair_table_gone: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM information_schema.tables
            WHERE table_name = 'campaign_evaluation_edge_pair_details')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(edge_pair_table_gone);

    // Note: 'edge_candidate' enum variant is documented as not removable via down.
    // Don't assert its absence.

    db.close().await;
}
