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
                "loss_ratio",
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
                "loss_threshold_ratio",
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

    db.close().await;
}

#[tokio::test]
async fn campaign_evaluation_relational_migration_shape() {
    // Guards the T54-02 migration: the JSONB `results` column + the
    // per-campaign UNIQUE constraint are both gone, the latest-per-
    // campaign index is present, the pair_detail_direct_source enum
    // exists with its two values, and the three child tables carry
    // the expected columns.
    let db = acquire(/*with_timescale=*/ false).await;
    meshmon_service::db::run_migrations(&db.pool)
        .await
        .expect("migrations apply");

    // 1. Enum exists with both values.
    let enum_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'pair_detail_direct_source')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(enum_exists, "pair_detail_direct_source enum missing");

    let enum_values: Vec<String> = sqlx::query_scalar(
        "SELECT e.enumlabel FROM pg_type t
           JOIN pg_enum e ON t.oid = e.enumtypid
          WHERE t.typname = 'pair_detail_direct_source'
          ORDER BY e.enumsortorder",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        enum_values,
        vec!["active_probe".to_string(), "vm_continuous".into()],
        "pair_detail_direct_source enum values mismatch",
    );

    // 2. Three new child tables exist with expected columns.
    for (table, expected_cols) in [
        (
            "campaign_evaluation_candidates",
            &[
                "evaluation_id",
                "destination_ip",
                "display_name",
                "city",
                "country_code",
                "asn",
                "network_operator",
                "is_mesh_member",
                "pairs_improved",
                "pairs_total_considered",
                "avg_improvement_ms",
            ][..],
        ),
        (
            "campaign_evaluation_pair_details",
            &[
                "evaluation_id",
                "candidate_destination_ip",
                "source_agent_id",
                "destination_agent_id",
                "direct_rtt_ms",
                "direct_stddev_ms",
                "direct_loss_ratio",
                "direct_source",
                "transit_rtt_ms",
                "transit_stddev_ms",
                "transit_loss_ratio",
                "improvement_ms",
                "qualifies",
                "mtr_measurement_id_ax",
                "mtr_measurement_id_xb",
            ][..],
        ),
        (
            "campaign_evaluation_unqualified_reasons",
            &["evaluation_id", "destination_ip", "reason"][..],
        ),
    ] {
        let cols: Vec<String> = sqlx::query_scalar(
            "SELECT column_name FROM information_schema.columns \
              WHERE table_name = $1 ORDER BY column_name",
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

    // 3. `campaign_evaluations.results` is gone.
    let has_results: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
          WHERE table_name = 'campaign_evaluations' AND column_name = 'results')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        !has_results,
        "campaign_evaluations.results column must be dropped",
    );

    // 4. Old per-campaign UNIQUE constraint is gone.
    let old_unique: bool = sqlx::query_scalar(
        "SELECT EXISTS (
           SELECT 1 FROM pg_constraint
            WHERE conname = 'campaign_evaluations_campaign_id_key'
         )",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        !old_unique,
        "campaign_evaluations_campaign_id_key UNIQUE constraint must be dropped",
    );

    // 5. New latest-per-campaign index is present.
    let new_index: bool = sqlx::query_scalar(
        "SELECT EXISTS (
           SELECT 1 FROM pg_indexes
            WHERE indexname = 'campaign_evaluations_campaign_evaluated_idx'
         )",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        new_index,
        "campaign_evaluations_campaign_evaluated_idx must exist",
    );

    db.close().await;
}

#[tokio::test]
async fn campaign_evaluation_guardrails_migration_up_down_round_trips() {
    // T55-I1: the four optional guardrail knobs land on
    // `measurement_campaigns` (operator-set) and
    // `campaign_evaluations` (snapshot stamped at /evaluate time).
    // All four columns are nullable with no default, so historic rows
    // backfill to NULL (knob disabled). The down migration drops them
    // again, leaving the post-T54 schema untouched.
    use sqlx::Executor;

    const GUARDRAIL_COLS: [&str; 4] = [
        "max_transit_rtt_ms",
        "max_transit_stddev_ms",
        "min_improvement_ms",
        "min_improvement_ratio",
    ];

    async fn column_exists(pool: &sqlx::PgPool, table: &str, col: &str) -> bool {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
               SELECT 1 FROM information_schema.columns
                WHERE table_name = $1 AND column_name = $2
             )",
        )
        .bind(table)
        .bind(col)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    let db = acquire(/*with_timescale=*/ false).await;
    meshmon_service::db::run_migrations(&db.pool)
        .await
        .expect("migrations apply");

    // --- After up migration: every guardrail column lands on both
    //     tables, nullable, no default.
    for table in ["measurement_campaigns", "campaign_evaluations"] {
        for col in GUARDRAIL_COLS {
            assert!(
                column_exists(&db.pool, table, col).await,
                "{table}.{col} missing after up migration",
            );

            // Nullable + no default — operator-disabled knob backfills
            // to NULL on historic rows.
            let (is_nullable, has_default): (String, bool) = sqlx::query_as(
                "SELECT is_nullable, column_default IS NOT NULL
                   FROM information_schema.columns
                  WHERE table_name = $1 AND column_name = $2",
            )
            .bind(table)
            .bind(col)
            .fetch_one(&db.pool)
            .await
            .unwrap();
            assert_eq!(is_nullable, "YES", "{table}.{col} must be nullable");
            assert!(!has_default, "{table}.{col} must not carry a DEFAULT");
        }
    }

    // --- Apply the down migration directly. The migrator only runs
    //     up-files; the down side ships in the same directory and is
    //     applied here as a sanity check for the round-trip shape.
    let down_sql =
        include_str!("../migrations/20260425000000_campaign_evaluation_guardrails.down.sql");
    db.pool
        .execute(down_sql)
        .await
        .expect("down migration applies");

    for table in ["measurement_campaigns", "campaign_evaluations"] {
        for col in GUARDRAIL_COLS {
            assert!(
                !column_exists(&db.pool, table, col).await,
                "{table}.{col} still present after down migration",
            );
        }
    }

    db.close().await;
}
