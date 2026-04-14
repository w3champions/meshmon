//! Shared test harness for migration integration tests.
//!
//! Each `acquire()` call spawns its own `timescale/timescaledb:latest-pg16`
//! container via `testcontainers` and returns a [`TestDb`] that owns the
//! container. When the `TestDb` is dropped (or its [`TestDb::close`] is
//! awaited) the container is stopped and removed by testcontainers.
//!
//! Using one container per test rather than sharing a single container via
//! a `static` keeps cleanup reliable: Rust statics never run `Drop`, so a
//! shared container would be orphaned on process exit.
//!
//! If `DATABASE_URL` is set the harness skips the container spawn and
//! targets that existing Postgres instead — useful for pointing at a
//! long-lived local Postgres during iterative development.

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::Executor;
use std::str::FromStr;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// Owns a freshly-created throwaway database plus (when auto-spawned) the
/// TimescaleDB container it lives in. Use [`TestDb::close`] to tear it down
/// explicitly, or just let the value go out of scope — `Drop` on
/// [`ContainerAsync`] stops the container.
pub struct TestDb {
    pub pool: PgPool,
    pub name: String,
    admin_opts: PgConnectOptions,
    /// `None` in `DATABASE_URL` mode; `Some` when we spawned a container.
    _container: Option<ContainerAsync<GenericImage>>,
}

impl TestDb {
    /// Close the pool and drop the test database. When the container is
    /// owned by this value, `Drop` stops and removes it right after.
    pub async fn close(self) {
        let Self {
            pool,
            name,
            admin_opts,
            _container,
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
        // `_container` drops here, which stops+removes the testcontainer.
    }
}

/// Acquire a fresh database for a migration test.
///
/// When `with_timescale` is `true`, installs `CREATE EXTENSION timescaledb`
/// in the new database so the TimescaleDB test path exercises hypertable
/// creation.
pub async fn acquire(with_timescale: bool) -> TestDb {
    let (admin_opts, container) = admin_options().await;

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
        _container: container,
    }
}

/// Resolve admin-DB connect options. Uses `DATABASE_URL` when set,
/// otherwise spawns a fresh TimescaleDB container and derives the URL from
/// its randomly-assigned host port.
async fn admin_options() -> (PgConnectOptions, Option<ContainerAsync<GenericImage>>) {
    if let Ok(url) = std::env::var("DATABASE_URL") {
        let opts = PgConnectOptions::from_str(&url).expect("parse DATABASE_URL");
        return (opts, None);
    }

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
    let opts = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("meshmon")
        .database("postgres");
    (opts, Some(container))
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
