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
        tracing::warn!(
            "timescaledb extension not found; continuing with plain Postgres \
             (route_snapshots will not be partitioned or compressed)"
        );
    }

    apply_grafana_role_password(pool).await?;
    Ok(())
}

/// Install or reinstall the TimescaleDB hypertable + compression + retention
/// policies for `route_snapshots`. Idempotent.
///
/// All four DDL statements run inside a single transaction on one
/// connection. Each individual `if_not_exists => TRUE` call is already
/// idempotent, but wrapping them atomically means a mid-sequence failure
/// (network blip, privilege edge case) leaves the DB in the pre-call state
/// rather than half-configured.
async fn apply_timescaledb_setup(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    // migrate_data => TRUE keeps this working when the operator installs
    // the timescaledb extension *after* the service has run for a while on
    // plain Postgres (route_snapshots already contains rows).
    sqlx::query(
        "SELECT create_hypertable(
            'route_snapshots',
            'observed_at',
            chunk_time_interval => INTERVAL '1 day',
            if_not_exists => TRUE,
            migrate_data => TRUE
        )",
    )
    .execute(&mut *tx)
    .await?;

    // ALTER TABLE ... SET ... is idempotent — reapplying the same storage
    // options is a no-op.
    sqlx::query(
        "ALTER TABLE route_snapshots SET (
            timescaledb.compress,
            timescaledb.compress_segmentby = 'source_id, target_id'
        )",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "SELECT add_compression_policy(
            'route_snapshots',
            INTERVAL '30 days',
            if_not_exists => TRUE
        )",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "SELECT add_retention_policy(
            'route_snapshots',
            INTERVAL '2 years',
            if_not_exists => TRUE
        )",
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await
}

/// Flip the `meshmon_grafana` role from NOLOGIN to LOGIN PASSWORD in a
/// single atomic ALTER ROLE.
///
/// The migration creates the role NOLOGIN so there is no window during
/// which the role is authenticatable without a credential. This helper
/// applies the LOGIN grant and the password in one statement.
///
/// Behaviour:
/// - `MESHMON_PG_GRAFANA_PASSWORD` unset → role stays NOLOGIN; log at
///   info. Grafana datasource calls fail loudly with "role ... is not
///   permitted to log in" if the operator later wires up Grafana
///   without setting the env var.
/// - `MESHMON_PG_GRAFANA_PASSWORD` set but empty → warn and leave
///   NOLOGIN.
/// - Set and non-empty → single atomic `ALTER ROLE ... WITH LOGIN
///   PASSWORD '...'`. Re-running rotates the password (idempotent).
async fn apply_grafana_role_password(pool: &PgPool) -> Result<(), sqlx::Error> {
    match std::env::var("MESHMON_PG_GRAFANA_PASSWORD") {
        Err(_) => {
            tracing::info!("MESHMON_PG_GRAFANA_PASSWORD not set; meshmon_grafana stays NOLOGIN");
            Ok(())
        }
        Ok(pw) if pw.is_empty() => {
            tracing::warn!(
                "MESHMON_PG_GRAFANA_PASSWORD is set but empty; \
                 meshmon_grafana stays NOLOGIN"
            );
            Ok(())
        }
        Ok(pw) => {
            // Safe dollar-quoting: ALTER ROLE ... WITH PASSWORD does
            // not accept parameter bindings, so inline quoting is the
            // only path.
            let quoted = pg_quote_dollar(&pw);

            // Roles are cluster-global. In integration-test scenarios
            // (shared Postgres container, many parallel `run_migrations`
            // calls against different databases) concurrent ALTER ROLE
            // statements on the same role race on `pg_authid` rows and
            // surface as `XX000: tuple concurrently updated`. Wrap the
            // ALTER in a transaction-scoped advisory lock keyed to the
            // same constant the up-migration uses, so every code path
            // that mutates this role serializes on the cluster.
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(4851623871)")
                .execute(&mut *tx)
                .await?;
            sqlx::query(&format!(
                "ALTER ROLE meshmon_grafana WITH LOGIN PASSWORD {quoted}"
            ))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::info!("meshmon_grafana role is LOGIN + password-protected");
            Ok(())
        }
    }
}

/// Dollar-quote a string as a Postgres string literal. Picks a
/// dollar-tag that doesn't appear in the input so the literal is safe
/// regardless of content.
fn pg_quote_dollar(s: &str) -> String {
    let mut tag = String::from("pw");
    while s.contains(&format!("${tag}$")) {
        tag.push('x');
    }
    format!("${tag}${s}${tag}$")
}

#[cfg(test)]
mod tests {
    use super::pg_quote_dollar;

    #[test]
    fn pg_quote_dollar_wraps_simple_value() {
        assert_eq!(pg_quote_dollar("simple"), "$pw$simple$pw$");
    }

    #[test]
    fn pg_quote_dollar_picks_nonconflicting_tag() {
        assert_eq!(pg_quote_dollar("$pw$boom$pw$"), "$pwx$$pw$boom$pw$$pwx$");
    }

    #[test]
    fn pg_quote_dollar_handles_embedded_quotes() {
        assert_eq!(pg_quote_dollar("p'a\"ss$w"), "$pw$p'a\"ss$w$pw$");
    }
}
