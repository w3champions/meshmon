//! Shared test harness for migration integration tests.
//!
//! On first `acquire()` call the harness lazy-spawns a single
//! `timescale/timescaledb:latest-pg16` container via `testcontainers` and
//! reuses it for the remainder of the test binary's life. Each individual
//! test then carves out its own throwaway database inside that container so
//! tests stay hermetic from one another.
//!
//! If `DATABASE_URL` is set in the environment, the harness uses that
//! instead of spawning a container — useful for pointing at a long-lived
//! local Postgres during iterative development.

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::Executor;
use std::str::FromStr;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::OnceCell;

/// Held for the lifetime of the test binary. Dropping it stops the container
/// (testcontainers registers its own `Drop` impl).
struct SharedContainer {
    _container: ContainerAsync<GenericImage>,
    url: String,
}

static CONTAINER: OnceCell<SharedContainer> = OnceCell::const_new();

/// Libpq URL of the admin (`postgres`) database inside either the shared
/// container or the `DATABASE_URL`-supplied server.
async fn admin_url() -> String {
    if let Ok(url) = std::env::var("DATABASE_URL") {
        return url;
    }
    CONTAINER
        .get_or_init(|| async {
            let container = GenericImage::new("timescale/timescaledb", "latest-pg16")
                .with_wait_for(WaitFor::message_on_stderr(
                    "database system is ready to accept connections",
                ))
                .with_exposed_port(ContainerPort::Tcp(5432))
                .with_env_var("POSTGRES_PASSWORD", "meshmon")
                .start()
                .await
                .expect("start timescaledb container — is Docker running?");
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("resolve container host port");
            let url = format!("postgres://postgres:meshmon@127.0.0.1:{port}/postgres");
            SharedContainer {
                _container: container,
                url,
            }
        })
        .await
        .url
        .clone()
}

/// Owns a freshly-created throwaway database. Use [`TestDb::close`] to tear
/// down the DB explicitly — `Drop` can't run `async` work, so tests that
/// panic will leave the DB behind (safe: the name is unique per invocation
/// and the parent container is discarded at process exit).
pub struct TestDb {
    pub pool: PgPool,
    pub name: String,
    admin_opts: PgConnectOptions,
}

impl TestDb {
    /// Close the pool and drop the underlying database.
    pub async fn close(self) {
        let Self {
            pool,
            name,
            admin_opts,
        } = self;
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

/// Acquire a fresh database for a migration test.
///
/// When `with_timescale` is `true`, installs `CREATE EXTENSION timescaledb`
/// in the new database so the TimescaleDB test path exercises hypertable
/// creation.
pub async fn acquire(with_timescale: bool) -> TestDb {
    let admin_url = admin_url().await;
    let admin_opts = PgConnectOptions::from_str(&admin_url).expect("parse admin URL");

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

    TestDb {
        pool,
        name: db_name,
        admin_opts,
    }
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
