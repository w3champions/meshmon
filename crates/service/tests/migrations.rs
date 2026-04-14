//! Migration integration tests.
//!
//! Every test gets its own `timescale/timescaledb` container spawned by the
//! `common` harness via `testcontainers`. Docker must be running locally;
//! GitHub Actions `ubuntu-latest` runners have Docker pre-installed.
//!
//! Setting `DATABASE_URL` skips the container spawn and targets the
//! supplied Postgres — useful for debugging against a long-lived dev DB.

mod common;

use meshmon_service::db::{detect_timescaledb, run_migrations};

#[tokio::test]
async fn migrations_apply_on_plain_postgres() {
    let db = common::acquire(false).await;

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
    let db = common::acquire(true).await;

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
    let db = common::acquire(true).await;

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
    let db = common::acquire(false).await;

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

/// Covers both post-migration constraint behaviors (FK restriction + CHECK
/// constraint on protocol) against the process-shared pre-migrated DB.
/// Each scenario wraps its work in a transaction that rolls back for
/// isolation — the same pattern T04+ DML tests should follow by default.
#[tokio::test]
async fn schema_constraints_behave_correctly() {
    let pool = common::shared_migrated_pool().await;

    // Scenario 1: ON DELETE RESTRICT blocks removal of an agent that still
    // has route_snapshots pointing at it.
    {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip) VALUES ('a', 'Agent A', '10.0.0.1')",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip) VALUES ('b', 'Agent B', '10.0.0.2')",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO route_snapshots
                (source_id, target_id, protocol, observed_at, hops)
             VALUES ('a', 'b', 'icmp', NOW(), '[]'::jsonb)",
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        let err = sqlx::query("DELETE FROM agents WHERE id = 'a'")
            .execute(&mut *tx)
            .await
            .expect_err("ON DELETE RESTRICT must block deletion");
        let msg = err.to_string();
        assert!(
            msg.contains("violates foreign key") || msg.contains("23503"),
            "expected FK violation, got: {msg}"
        );
        tx.rollback().await.unwrap();
    }

    // Scenario 2: CHECK constraint rejects unknown protocol values.
    {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip) VALUES ('a', 'Agent A', '10.0.0.1')",
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        let err = sqlx::query(
            "INSERT INTO route_snapshots
                (source_id, target_id, protocol, observed_at, hops)
             VALUES ('a', 'a', 'sctp', NOW(), '[]'::jsonb)",
        )
        .execute(&mut *tx)
        .await
        .expect_err("CHECK constraint must reject 'sctp'");
        let msg = err.to_string();
        assert!(
            msg.contains("check constraint") || msg.contains("23514"),
            "expected CHECK violation, got: {msg}"
        );
        tx.rollback().await.unwrap();
    }
}
