//! Shared test harness for migration integration tests.
//!
//! Reads `DATABASE_URL` (skipping the calling test if unset), carves out a
//! throwaway database with a random suffix, and returns a pool connected to
//! it plus a drop-guard that tears the database down on test exit.

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::Executor;
use std::str::FromStr;

/// Owns a freshly-created throwaway database. Use [`TestDb::close`] to tear
/// down the DB explicitly — `Drop` can't run `async` work, so tests that
/// panic will leave the DB behind (safe: the name is unique per invocation).
pub struct TestDb {
    pub pool: PgPool,
    pub name: String,
    admin_opts: PgConnectOptions,
}

impl TestDb {
    /// Close the pool and drop the underlying database.
    pub async fn close(self) {
        let Self { pool, name, admin_opts } = self;
        pool.close().await;
        let admin = PgPool::connect_with(admin_opts)
            .await
            .expect("connect to admin db for teardown");
        // WITH (FORCE) terminates any lingering sessions before dropping,
        // protecting against teardown flakes on Postgres 13+.
        let _ = admin
            .execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
            .await;
        admin.close().await;
    }
}

/// Acquire (or skip) a fresh database for a migration test.
///
/// Returns `None` if `DATABASE_URL` is unset — caller should `return` to
/// skip the test. Otherwise, creates a throwaway DB and, when
/// `with_timescale` is true, installs `CREATE EXTENSION timescaledb` in it.
pub async fn acquire(with_timescale: bool) -> Option<TestDb> {
    let admin_url = std::env::var("DATABASE_URL").ok()?;
    let admin_opts = PgConnectOptions::from_str(&admin_url).expect("parse DATABASE_URL");

    // A random suffix survives parallel `cargo test` workers. 16 hex chars
    // is plenty of entropy for a short-lived test DB.
    let db_name = format!("meshmon_t03_{:016x}", rand_u64());

    let admin = PgPool::connect_with(admin_opts.clone())
        .await
        .expect("connect to admin db");
    // TEMPLATE template0 avoids inheriting extensions that the TimescaleDB
    // docker image pre-installs into template1. Without this, every new DB
    // would already have timescaledb loaded and the plain-Postgres test
    // matrix would be untestable.
    admin
        .execute(format!("CREATE DATABASE \"{db_name}\" TEMPLATE template0").as_str())
        .await
        .expect("create test database");
    admin.close().await;

    // Building PgConnectOptions directly avoids any URL round-tripping loss
    // (sslmode, application_name, etc.).
    let test_opts = admin_opts.clone().database(&db_name);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(test_opts)
        .await
        .expect("connect to fresh test database");

    if with_timescale {
        pool.execute("CREATE EXTENSION IF NOT EXISTS timescaledb")
            .await
            .expect(
                "install timescaledb extension — is the Postgres image \
                 timescale/timescaledb?",
            );
    }

    Some(TestDb {
        pool,
        name: db_name,
        admin_opts,
    })
}

/// Mix the current time with a hash of the thread id to get a unique-ish
/// 64-bit value. Not cryptographic; we only need "won't collide between
/// concurrent test threads on the same tick."
fn rand_u64() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut h = DefaultHasher::new();
    std::thread::current().id().hash(&mut h);
    nanos ^ h.finish().wrapping_mul(0x9E37_79B9_7F4A_7C15)
}
