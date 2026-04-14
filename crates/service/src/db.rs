//! Postgres pool, extension detection, and migration runner.
//!
//! Public surface:
//! - [`connect`] — open a pooled Postgres connection with sensible defaults.
//! - [`detect_timescaledb`] — `true` iff the `timescaledb` extension is
//!   installed in the connected database.
//! - [`run_migrations`] — apply the embedded sqlx migrations and, when
//!   TimescaleDB is available, install the hypertable + compression +
//!   retention policies for `route_snapshots`.
//!
//! `run_migrations` is idempotent: calling it repeatedly is a no-op once the
//! schema is already at the latest version. This lets T04 call it
//! unconditionally at every service startup.

use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Embedded migrations read from `crates/service/migrations/`.
///
/// Uses the compile-time form so the binary doesn't need a filesystem
/// migrations directory at runtime. `Migrator` tracks applied versions in the
/// `_sqlx_migrations` bookkeeping table it creates on first run.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Pool size. Picked to comfortably cover the ingestion pipeline (T07) plus a
/// handful of concurrent user-facing reads without over-saturating Postgres.
/// T04 may expose this via config; for now it's a constant.
const DEFAULT_POOL_SIZE: u32 = 16;

/// Open a pooled Postgres connection.
///
/// `url` follows the standard libpq form:
/// `postgres://user:pass@host:port/dbname?sslmode=require`.
pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(DEFAULT_POOL_SIZE)
        .connect(url)
        .await
}

/// Return `true` if the `timescaledb` extension is installed in the
/// connected database.
///
/// Note: this checks the *current database*, not the cluster. The extension
/// binary may be loadable by the server (e.g. the `timescale/timescaledb`
/// Docker image) without being installed in every database on it.
pub async fn detect_timescaledb(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let (present,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb')")
            .fetch_one(pool)
            .await?;
    Ok(present)
}

/// Apply the embedded sqlx migrations, then — if TimescaleDB is present —
/// install the hypertable, compression policy, and retention policy for
/// `route_snapshots`.
///
/// Safe to call on every service startup: sqlx only runs outstanding
/// migrations, and the TimescaleDB DDL below uses `if_not_exists => TRUE` so
/// repeated invocations are no-ops.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    MIGRATOR.run(pool).await?;

    if detect_timescaledb(pool).await? {
        apply_timescaledb_setup(pool).await?;
    } else {
        // T04 swaps this for `tracing::warn!`. eprintln! keeps T03 self-
        // contained without pulling tracing into the dep graph yet.
        eprintln!(
            "meshmon: timescaledb extension not found; continuing with plain \
             Postgres (route_snapshots will not be partitioned or compressed)"
        );
    }
    Ok(())
}

/// Install or reinstall the TimescaleDB hypertable + compression + retention
/// policies for `route_snapshots`. Idempotent.
async fn apply_timescaledb_setup(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT create_hypertable(
            'route_snapshots',
            'observed_at',
            chunk_time_interval => INTERVAL '1 day',
            if_not_exists => TRUE
        )",
    )
    .execute(pool)
    .await?;

    // ALTER TABLE ... SET ... is idempotent — reapplying the same storage
    // options is a no-op.
    sqlx::query(
        "ALTER TABLE route_snapshots SET (
            timescaledb.compress,
            timescaledb.compress_segmentby = 'source_id, target_id'
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "SELECT add_compression_policy(
            'route_snapshots',
            INTERVAL '30 days',
            if_not_exists => TRUE
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "SELECT add_retention_policy(
            'route_snapshots',
            INTERVAL '2 years',
            if_not_exists => TRUE
        )",
    )
    .execute(pool)
    .await?;

    Ok(())
}
