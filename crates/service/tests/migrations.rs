//! Migration integration tests.
//!
//! Requires `DATABASE_URL` to point at a Postgres (ideally with the
//! `timescaledb` extension available, i.e. the `timescale/timescaledb` image)
//! with permissions to `CREATE DATABASE` and install extensions. Each test
//! carves out its own database so runs are hermetic.
//!
//! When `DATABASE_URL` is unset, every test short-circuits to `return` — this
//! matches the workspace convention that a missing DB environment should not
//! fail CI for crates that don't touch Postgres.

mod common;

use meshmon_service::db::{detect_timescaledb, run_migrations};

#[tokio::test]
async fn migrations_apply_on_plain_postgres() {
    let Some(db) = common::acquire(false).await else {
        return;
    };

    // Sanity: extension is *not* installed in this DB.
    assert!(!detect_timescaledb(&db.pool).await.unwrap());

    run_migrations(&db.pool)
        .await
        .expect("run_migrations on plain Postgres");

    // Tables exist.
    let agents_exists: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_schema = 'public' AND table_name = 'agents'
        )",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(agents_exists.0, "agents table must exist");

    let snaps_exists: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_schema = 'public' AND table_name = 'route_snapshots'
        )",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(snaps_exists.0, "route_snapshots table must exist");

    // Indexes exist.
    for idx in [
        "idx_agents_last_seen",
        "idx_route_snapshots_lookup",
        "idx_route_snapshots_hops_gin",
    ] {
        let present: (bool,) = sqlx::query_as(
            "SELECT EXISTS (
                SELECT 1 FROM pg_indexes
                WHERE schemaname = 'public' AND indexname = $1
            )",
        )
        .bind(idx)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(present.0, "index {idx} must exist");
    }

    // No hypertable should have been created on plain Postgres.
    let hyper_exists: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_schema = '_timescaledb_catalog' AND table_name = 'hypertable'
        )",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        !hyper_exists.0,
        "TimescaleDB catalog must not exist on plain Postgres"
    );

    db.close().await;
}

#[tokio::test]
async fn migrations_apply_on_timescaledb() {
    let Some(db) = common::acquire(true).await else {
        return;
    };

    assert!(detect_timescaledb(&db.pool).await.unwrap());

    run_migrations(&db.pool)
        .await
        .expect("run_migrations on TimescaleDB");

    // Hypertable has been created.
    let is_hyper: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1
            FROM timescaledb_information.hypertables
            WHERE hypertable_name = 'route_snapshots'
        )",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(is_hyper.0, "route_snapshots must be a hypertable");

    // Compression is enabled.
    let compress_on: (bool,) = sqlx::query_as(
        "SELECT compression_enabled
         FROM timescaledb_information.hypertables
         WHERE hypertable_name = 'route_snapshots'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(compress_on.0, "compression must be enabled");

    // Compression + retention jobs are registered.
    let jobs: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint
         FROM timescaledb_information.jobs
         WHERE hypertable_name = 'route_snapshots'
           AND proc_name IN ('policy_compression', 'policy_retention')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        jobs.0, 2,
        "expected compression + retention policies to be scheduled"
    );

    db.close().await;
}

#[tokio::test]
async fn migrations_are_idempotent_on_timescaledb() {
    let Some(db) = common::acquire(true).await else {
        return;
    };

    run_migrations(&db.pool).await.expect("first run");
    run_migrations(&db.pool)
        .await
        .expect("second run must be a no-op, not an error");

    // Still exactly one compression + one retention job.
    let jobs: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint
         FROM timescaledb_information.jobs
         WHERE hypertable_name = 'route_snapshots'
           AND proc_name IN ('policy_compression', 'policy_retention')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(jobs.0, 2, "idempotent re-run must not duplicate policies");

    db.close().await;
}

#[tokio::test]
async fn migrations_are_idempotent_on_plain_postgres() {
    let Some(db) = common::acquire(false).await else {
        return;
    };

    run_migrations(&db.pool).await.expect("first run");
    run_migrations(&db.pool).await.expect("second run");

    // sqlx tracks applied migrations — exactly one version recorded.
    let applied: (i64,) = sqlx::query_as("SELECT COUNT(*)::bigint FROM _sqlx_migrations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(applied.0, 1, "exactly one migration should be recorded");

    db.close().await;
}

#[tokio::test]
async fn foreign_keys_restrict_agent_delete() {
    let Some(db) = common::acquire(false).await else {
        return;
    };
    run_migrations(&db.pool).await.unwrap();

    sqlx::query("INSERT INTO agents (id, display_name, ip) VALUES ('a', 'Agent A', '10.0.0.1')")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id, display_name, ip) VALUES ('b', 'Agent B', '10.0.0.2')")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO route_snapshots
            (source_id, target_id, protocol, observed_at, hops)
         VALUES ('a', 'b', 'icmp', NOW(), '[]'::jsonb)",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let err = sqlx::query("DELETE FROM agents WHERE id = 'a'")
        .execute(&db.pool)
        .await
        .expect_err("ON DELETE RESTRICT must block deletion");
    let msg = err.to_string();
    assert!(
        msg.contains("violates foreign key") || msg.contains("23503"),
        "expected FK violation, got: {msg}"
    );

    db.close().await;
}

#[tokio::test]
async fn protocol_check_constraint_rejects_unknown_value() {
    let Some(db) = common::acquire(false).await else {
        return;
    };
    run_migrations(&db.pool).await.unwrap();

    sqlx::query("INSERT INTO agents (id, display_name, ip) VALUES ('a', 'Agent A', '10.0.0.1')")
        .execute(&db.pool)
        .await
        .unwrap();

    let err = sqlx::query(
        "INSERT INTO route_snapshots
            (source_id, target_id, protocol, observed_at, hops)
         VALUES ('a', 'a', 'sctp', NOW(), '[]'::jsonb)",
    )
    .execute(&db.pool)
    .await
    .expect_err("CHECK constraint must reject 'sctp'");
    let msg = err.to_string();
    assert!(
        msg.contains("check constraint") || msg.contains("23514"),
        "expected CHECK violation, got: {msg}"
    );

    db.close().await;
}
