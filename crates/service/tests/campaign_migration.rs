mod common;

use common::acquire;

#[tokio::test]
async fn migration_creates_campaign_tables_and_indexes() {
    let db = acquire(/*with_timescale=*/ false).await;
    meshmon_service::db::run_migrations(&db.pool)
        .await
        .expect("migrations apply");

    // ENUMs exist.
    for ty in [
        "probe_protocol",
        "campaign_state",
        "pair_resolution_state",
        "evaluation_mode",
        "measurement_kind",
    ] {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_type WHERE typname = $1)")
                .bind(ty)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(exists, "enum {ty} not created");
    }

    // Tables exist with expected columns.
    for (table, expected_cols) in [
        (
            "measurements",
            &[
                "id",
                "source_agent_id",
                "destination_ip",
                "protocol",
                "probe_count",
                "measured_at",
                "latency_min_ms",
                "latency_avg_ms",
                "latency_median_ms",
                "latency_p95_ms",
                "latency_max_ms",
                "latency_stddev_ms",
                "loss_pct",
                "kind",
            ][..],
        ),
        (
            "measurement_campaigns",
            &[
                "id",
                "title",
                "notes",
                "state",
                "protocol",
                "probe_count",
                "probe_count_detail",
                "timeout_ms",
                "probe_stagger_ms",
                "force_measurement",
                "loss_threshold_pct",
                "stddev_weight",
                "evaluation_mode",
                "created_by",
                "created_at",
                "started_at",
                "stopped_at",
                "completed_at",
                "evaluated_at",
            ][..],
        ),
        (
            "campaign_pairs",
            &[
                "id",
                "campaign_id",
                "source_agent_id",
                "destination_ip",
                "resolution_state",
                "measurement_id",
                "dispatched_at",
                "settled_at",
                "attempt_count",
                "last_error",
            ][..],
        ),
    ] {
        let cols: Vec<String> = sqlx::query_scalar(
            "SELECT column_name FROM information_schema.columns WHERE table_name = $1 ORDER BY column_name",
        )
        .bind(table)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        for c in expected_cols {
            assert!(
                cols.iter().any(|x| x == c),
                "{table} missing column {c}; got {cols:?}"
            );
        }
    }

    // NOTIFY trigger exists.
    let trigger_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname = 'measurement_campaigns_notify')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(trigger_exists, "NOTIFY trigger missing");

    // Unique constraint on (campaign_id, source_agent_id, destination_ip).
    let cid: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO measurement_campaigns (title, protocol) VALUES ('t', 'icmp') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO campaign_pairs (campaign_id, source_agent_id, destination_ip)
         VALUES ($1, 'agent-a', '203.0.113.7')",
    )
    .bind(cid)
    .execute(&db.pool)
    .await
    .unwrap();
    let err = sqlx::query(
        "INSERT INTO campaign_pairs (campaign_id, source_agent_id, destination_ip)
         VALUES ($1, 'agent-a', '203.0.113.7')",
    )
    .bind(cid)
    .execute(&db.pool)
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("campaign_pairs"),
        "duplicate insert must fail: {err}"
    );

    db.close().await;
}

#[tokio::test]
async fn campaign_evaluations_migration_applies_cleanly() {
    let db = acquire(/*with_timescale=*/ false).await;
    meshmon_service::db::run_migrations(&db.pool)
        .await
        .expect("migrations apply");

    let has_kind: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
         WHERE table_name = 'campaign_pairs' AND column_name = 'kind')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(has_kind, "campaign_pairs.kind column missing");

    let has_tbl: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables
         WHERE table_name = 'campaign_evaluations')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(has_tbl, "campaign_evaluations table missing");

    let unique_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
           SELECT 1 FROM pg_constraint
           WHERE conrelid = 'campaign_evaluations'::regclass
             AND contype = 'u'
         )",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        unique_exists,
        "campaign_evaluations UNIQUE constraint missing"
    );

    db.close().await;
}
