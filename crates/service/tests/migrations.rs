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

    // Hold the grafana test lock while migrations run — migrations
    // touch the cluster-level `meshmon_grafana` role, which races with
    // the grafana-specific tests. See `GRAFANA_TEST_LOCK` below.
    {
        let _guard = GRAFANA_TEST_LOCK.lock().await;
        std::env::remove_var(GRAFANA_PASSWORD_ENV);
        run_migrations(&db.pool)
            .await
            .expect("run_migrations on plain Postgres");
    }

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

    {
        let _guard = GRAFANA_TEST_LOCK.lock().await;
        std::env::remove_var(GRAFANA_PASSWORD_ENV);
        run_migrations(&db.pool)
            .await
            .expect("run_migrations on TimescaleDB");
    }

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

    {
        let _guard = GRAFANA_TEST_LOCK.lock().await;
        std::env::remove_var(GRAFANA_PASSWORD_ENV);
        run_migrations(&db.pool).await.expect("first run");
        run_migrations(&db.pool)
            .await
            .expect("second run must be a no-op, not an error");
    }

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

    {
        let _guard = GRAFANA_TEST_LOCK.lock().await;
        std::env::remove_var(GRAFANA_PASSWORD_ENV);
        run_migrations(&db.pool).await.expect("first run");
        run_migrations(&db.pool).await.expect("second run");
    }

    // sqlx tracks applied migrations — the count must match the number of
    // files actually committed under `crates/service/migrations/` (grows by
    // one every migration we add). Counting versions on disk beats hard-
    // coding the number here, which silently drifts with every new
    // migration.
    let expected_versions = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .expect("read migrations dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".up.sql"))
        .count() as i64;

    let applied: (i64,) = sqlx::query_as("SELECT COUNT(*)::bigint FROM _sqlx_migrations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        applied.0, expected_versions,
        "one row per `*.up.sql` migration should be recorded, re-run must be idempotent",
    );

    db.close().await;
}

/// Serializes every `run_migrations` call in this test binary.
///
/// `apply_grafana_role_password` reads `MESHMON_PG_GRAFANA_PASSWORD`
/// from the process environment, and the `meshmon_grafana` role lives
/// at the Postgres *cluster* level (visible across every database in
/// the shared test container). Two axes of shared state therefore
/// break parallelism:
///
/// 1. `std::env::{set_var,remove_var}` mutates process-global state,
///    so an env-var-mutating test races with any parallel reader.
/// 2. Concurrent `ALTER ROLE meshmon_grafana ...` statements race on
///    `pg_authid` rows and surface as `XX000: tuple concurrently
///    updated`.
///
/// Holding a single mutex across *every* `run_migrations` call in this
/// binary — grafana-specific tests AND the broader
/// `migrations_apply_on_*` / `migrations_are_idempotent_on_*` tests —
/// eliminates both races. Per-test DB isolation still lets the
/// non-migration bodies run in parallel; only the migrations are
/// serialized.
static GRAFANA_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const GRAFANA_PASSWORD_ENV: &str = "MESHMON_PG_GRAFANA_PASSWORD";

/// Run `run_migrations` with the grafana lock held and the env var
/// forced to a known state (`Some(pw)` → set; `None` → cleared). The
/// lock is released before returning, which is fine for tests that
/// don't subsequently assert on the cluster-level role state. Tests
/// that do assert on role state must keep the guard alive themselves.
async fn run_migrations_locked_with_env(
    pool: &sqlx::PgPool,
    env: Option<&str>,
) -> Result<tokio::sync::MutexGuard<'static, ()>, sqlx::Error> {
    let guard = GRAFANA_TEST_LOCK.lock().await;
    match env {
        Some(pw) => std::env::set_var(GRAFANA_PASSWORD_ENV, pw),
        None => std::env::remove_var(GRAFANA_PASSWORD_ENV),
    }
    run_migrations(pool).await?;
    // Clear the env var eagerly so that if this guard is dropped and a
    // non-grafana test runs next, it doesn't accidentally pick up a
    // stale `set_var`.
    std::env::remove_var(GRAFANA_PASSWORD_ENV);
    Ok(guard)
}

#[tokio::test]
async fn run_migrations_creates_grafana_role_nologin_by_default() {
    // Uses own_container() because this test asserts on cluster-level role
    // state (pg_roles). Other test binaries may call run_migrations on the
    // shared cluster concurrently and flip meshmon_grafana's LOGIN attribute,
    // breaking the assertion. An isolated container eliminates that race.
    let db = common::own_container().await;

    // Hold the guard across the assertions so a sibling test within this
    // binary can't flip the env var between our `run_migrations` and our
    // `rolcanlogin` query.
    let _guard = run_migrations_locked_with_env(&db.pool, None)
        .await
        .expect("run_migrations must succeed without the env var");

    // Role exists.
    let role_exists: (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'meshmon_grafana')")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(role_exists.0, "meshmon_grafana role must be created");

    // Role is NOLOGIN.
    let can_login: (bool,) =
        sqlx::query_as("SELECT rolcanlogin FROM pg_roles WHERE rolname = 'meshmon_grafana'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        !can_login.0,
        "meshmon_grafana must remain NOLOGIN when env var is unset"
    );

    db.close().await;
}

#[tokio::test]
async fn run_migrations_flips_grafana_role_to_login_when_password_set() {
    // Uses own_container() — this test asserts on cluster-level role state
    // (pg_roles, pg_authid) and sets MESHMON_PG_GRAFANA_PASSWORD to LOGIN.
    // An isolated container prevents the LOGIN state from racing with other
    // binaries that assert NOLOGIN on the shared cluster.
    let db = common::own_container().await;

    let _guard = run_migrations_locked_with_env(&db.pool, Some("s3cret$pw$value"))
        .await
        .expect("run_migrations with env var set must succeed");

    let can_login: (bool,) =
        sqlx::query_as("SELECT rolcanlogin FROM pg_roles WHERE rolname = 'meshmon_grafana'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        can_login.0,
        "meshmon_grafana must be LOGIN once MESHMON_PG_GRAFANA_PASSWORD is applied"
    );

    // A password has been set — pg_authid.rolpassword is non-null for
    // md5/scram-stored credentials. Superuser is required to read it,
    // and the test container runs as `postgres`, so this check works.
    let has_password: (bool,) = sqlx::query_as(
        "SELECT rolpassword IS NOT NULL
         FROM pg_authid WHERE rolname = 'meshmon_grafana'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(has_password.0, "meshmon_grafana must have a password set");

    // No need to reset the role — this container is dedicated to this test
    // and will be torn down by db.close().
    db.close().await;
}

#[tokio::test]
async fn run_migrations_revokes_grafana_login_when_env_var_removed() {
    // Security-regression test: rotating the secret by removing the
    // env var and restarting must actually disable DB login for the
    // Grafana role. Silently leaving it LOGIN + old-password would
    // defeat the documented operator knob.
    //
    // Uses own_container() because this test performs two sequential
    // run_migrations calls with different env var states and asserts the role
    // transitions correctly. Sharing the cluster with other binaries would
    // allow a concurrent ALTER ROLE to interfere between the two runs.
    let db = common::own_container().await;

    // Arrange: flip to LOGIN + password first …
    let _guard = run_migrations_locked_with_env(&db.pool, Some("rotate-me"))
        .await
        .expect("first run (password set) must succeed");
    let logged_in: (bool,) =
        sqlx::query_as("SELECT rolcanlogin FROM pg_roles WHERE rolname = 'meshmon_grafana'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(logged_in.0, "precondition: role should be LOGIN after set");
    // Release the guard so the next call can reacquire + mutate env.
    drop(_guard);

    // Act: restart-equivalent with the env var removed.
    let _guard = run_migrations_locked_with_env(&db.pool, None)
        .await
        .expect("second run (password unset) must succeed");

    // Assert: role is back to NOLOGIN.
    let can_login: (bool,) =
        sqlx::query_as("SELECT rolcanlogin FROM pg_roles WHERE rolname = 'meshmon_grafana'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        !can_login.0,
        "removing MESHMON_PG_GRAFANA_PASSWORD must revoke LOGIN on meshmon_grafana"
    );

    db.close().await;
}

#[tokio::test]
async fn grafana_role_has_select_but_not_insert() {
    // Uses own_container() because this test asserts on cluster-level role
    // privileges (has_table_privilege, pg_roles). An isolated container
    // prevents concurrent role DDL from other test binaries from racing with
    // the privilege assertions.
    let db = common::own_container().await;

    let _guard = run_migrations_locked_with_env(&db.pool, None)
        .await
        .expect("run_migrations must succeed");

    // SELECT on agents is granted.
    let can_select_agents: (bool,) =
        sqlx::query_as("SELECT has_table_privilege('meshmon_grafana', 'public.agents', 'SELECT')")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        can_select_agents.0,
        "meshmon_grafana must have SELECT on agents"
    );

    // SELECT on route_snapshots is granted.
    let can_select_snaps: (bool,) = sqlx::query_as(
        "SELECT has_table_privilege('meshmon_grafana', 'public.route_snapshots', 'SELECT')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        can_select_snaps.0,
        "meshmon_grafana must have SELECT on route_snapshots"
    );

    // INSERT/UPDATE/DELETE must NOT be granted.
    for priv_name in ["INSERT", "UPDATE", "DELETE"] {
        let has_priv: (bool,) =
            sqlx::query_as("SELECT has_table_privilege('meshmon_grafana', 'public.agents', $1)")
                .bind(priv_name)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(
            !has_priv.0,
            "meshmon_grafana must NOT have {priv_name} on agents"
        );

        let has_priv_snaps: (bool,) = sqlx::query_as(
            "SELECT has_table_privilege('meshmon_grafana', 'public.route_snapshots', $1)",
        )
        .bind(priv_name)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(
            !has_priv_snaps.0,
            "meshmon_grafana must NOT have {priv_name} on route_snapshots"
        );
    }

    db.close().await;
}

/// Covers both post-migration constraint behaviors (FK restriction + CHECK
/// constraint on protocol) against the process-shared pre-migrated DB.
/// Each scenario wraps its work in a transaction that rolls back for
/// isolation — the same pattern T04+ DML tests should follow by default.
#[tokio::test]
async fn schema_constraints_behave_correctly() {
    // The first caller of `shared_migrated_pool` in this binary triggers its
    // internal `run_migrations`, which touches the cluster-level
    // `meshmon_grafana` role. Take the lock to serialize against sibling
    // tests in this binary (e.g. migrations_apply_on_*) that also call
    // run_migrations with remove_var(GRAFANA_PASSWORD_ENV). The grafana-role
    // assertion tests now use own_container() so they don't contend here.
    let pool = {
        let _guard = GRAFANA_TEST_LOCK.lock().await;
        std::env::remove_var(GRAFANA_PASSWORD_ENV);
        common::shared_migrated_pool().await
    };

    // Scenario 1: ON DELETE RESTRICT blocks removal of an agent that still
    // has route_snapshots pointing at it.
    {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
             VALUES ('a', 'Agent A', '10.0.0.1', 3555, 3552)",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
             VALUES ('b', 'Agent B', '10.0.0.2', 3555, 3552)",
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
            "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
             VALUES ('a', 'Agent A', '10.0.0.1', 3555, 3552)",
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
