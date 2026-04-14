//! Integration-test harness for the meshmon service.
//!
//! # Design
//!
//! A single `timescale/timescaledb` container is spawned per test binary on
//! first use and shared across every test in that binary. Tests get
//! isolation at one of two granularities:
//!
//! 1. **Fresh database per test** via [`acquire`] — for tests that need to
//!    own the schema (migration tests, schema-evolution tests, anything
//!    that runs DDL).
//! 2. **Shared pre-migrated database + transaction rollback** via
//!    [`shared_migrated_pool`] — for DML-only tests (most tests in T04+
//!    will fall here: writers, readers, HTTP handlers). Each test wraps its
//!    work in `pool.begin().await?` and rolls back for isolation.
//!
//! The shared container is stopped and removed at process exit by a
//! [`ctor::dtor`] hook. This is necessary because Rust statics never run
//! `Drop`, so the `ContainerAsync` held in a `OnceCell` would otherwise
//! leak every time `cargo test` finishes.
//!
//! Setting `DATABASE_URL` bypasses the container spawn and targets the
//! supplied Postgres. Useful for iterating against a long-lived local
//! server or for reproducing issues against a remote DB. When in override
//! mode the harness does not own the server, so the `#[dtor]` is a no-op.
//!
//! # Example: DDL-owning test (migration, schema evolution)
//!
//! ```ignore
//! #[tokio::test]
//! async fn my_migration_test() {
//!     let db = common::acquire(/*with_timescale=*/ false).await;
//!     meshmon_service::db::run_migrations(&db.pool).await.unwrap();
//!     // ... assertions against db.pool ...
//!     db.close().await;
//! }
//! ```
//!
//! # Example: DML-only test (default pattern going forward)
//!
//! ```ignore
//! #[tokio::test]
//! async fn my_dml_test() {
//!     let pool = common::shared_migrated_pool().await;
//!     let mut tx = pool.begin().await.unwrap();
//!     sqlx::query("INSERT INTO agents (id, display_name, ip) \
//!                  VALUES ('a', 'Agent A', '10.0.0.1')")
//!         .execute(&mut *tx).await.unwrap();
//!     // ... more work on &mut *tx ...
//!     tx.rollback().await.unwrap();
//! }
//! ```

use ctor::dtor;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::Executor;
use std::str::FromStr;
use std::sync::Mutex;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::OnceCell;
use uuid::Uuid;

/// Pinned TimescaleDB image. Rolling tags (`latest`, `latest-pg16`) drift
/// silently and break historical reproducibility, so this is a deliberate
/// bump-when-you-mean-it constant.
const TIMESCALEDB_IMAGE: &str = "timescale/timescaledb";
const TIMESCALEDB_TAG: &str = "2.26.3-pg16";

/// Pool size for the shared pre-migrated pool. Generous enough that dozens
/// of parallel tests can each hold a transaction without blocking.
const SHARED_POOL_MAX_CONNECTIONS: u32 = 32;

struct SharedContainer {
    /// Admin-DB connect options (`postgres` database on the spawned server,
    /// or the parsed `DATABASE_URL` when in override mode).
    admin_opts: PgConnectOptions,
    /// `None` in `DATABASE_URL` mode. Populated when we own the container.
    /// Held here so its lifetime spans every test in the binary; the
    /// `#[dtor]` below only reads the container's `id()` and shells out to
    /// `docker rm -f`, so the `ContainerAsync` itself is never `Drop`ped.
    /// That's fine: we just need the server stopped.
    container: Mutex<Option<ContainerAsync<GenericImage>>>,
}

static SHARED: OnceCell<SharedContainer> = OnceCell::const_new();
static SHARED_MIGRATED: OnceCell<PgPool> = OnceCell::const_new();

async fn shared() -> &'static SharedContainer {
    SHARED
        .get_or_init(|| async {
            if let Ok(url) = std::env::var("DATABASE_URL") {
                return SharedContainer {
                    admin_opts: PgConnectOptions::from_str(&url).expect("parse DATABASE_URL"),
                    container: Mutex::new(None),
                };
            }
            let container = GenericImage::new(TIMESCALEDB_IMAGE, TIMESCALEDB_TAG)
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
            let admin_opts = PgConnectOptions::new()
                .host("127.0.0.1")
                .port(port)
                .username("postgres")
                .password("meshmon")
                .database("postgres");
            SharedContainer {
                admin_opts,
                container: Mutex::new(Some(container)),
            }
        })
        .await
}

/// Owns a freshly-created throwaway database inside the shared container
/// (or the `DATABASE_URL`-supplied server). Call [`TestDb::close`] to drop
/// the DB when the test finishes — the shared container survives across
/// tests and is cleaned up at process exit.
pub struct TestDb {
    pub pool: PgPool,
    pub name: String,
    admin_opts: PgConnectOptions,
}

impl TestDb {
    /// Drop the test database. Always safe to call; `DROP DATABASE ...
    /// WITH (FORCE)` terminates any lingering sessions (Postgres 13+).
    pub async fn close(self) {
        let Self {
            pool,
            name,
            admin_opts,
        } = self;
        pool.close().await;
        let admin = PgPool::connect_with(admin_opts)
            .await
            .expect("connect admin for teardown");
        let _ = admin
            .execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
            .await;
        admin.close().await;
    }
}

/// Acquire a fresh, isolated Postgres database for a DDL-owning test.
///
/// The database is created inside the process-shared TimescaleDB container
/// (or inside the `DATABASE_URL`-supplied server). When `with_timescale`
/// is `true`, the `timescaledb` extension is installed in the new database
/// so tests can exercise hypertable creation. `TEMPLATE template0` keeps
/// the new database free of extensions inherited from `template1`.
///
/// Callers should invoke [`TestDb::close`] when done. Forgetting to do so
/// leaks the database inside the shared server for the rest of the test
/// binary's lifetime, then the `#[dtor]` tears down the whole container.
pub async fn acquire(with_timescale: bool) -> TestDb {
    let shared = shared().await;
    let db_name = format!("meshmon_t03_{}", Uuid::new_v4().simple());

    let admin = PgPool::connect_with(shared.admin_opts.clone())
        .await
        .expect("connect admin");
    admin
        .execute(format!("CREATE DATABASE \"{db_name}\" TEMPLATE template0").as_str())
        .await
        .expect("create test database");
    admin.close().await;

    let test_opts = shared.admin_opts.clone().database(&db_name);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(test_opts)
        .await
        .expect("connect test db");

    if with_timescale {
        pool.execute("CREATE EXTENSION IF NOT EXISTS timescaledb")
            .await
            .expect("install timescaledb");
    }

    TestDb {
        pool,
        name: db_name,
        admin_opts: shared.admin_opts.clone(),
    }
}

/// Return a pool pointing at the process-shared, pre-migrated Postgres
/// database.
///
/// Use this for DML-only tests: open a `tx = pool.begin().await?`, do
/// inserts/updates/selects against `&mut *tx`, and either `rollback()` (to
/// leave the shared DB untouched) or `commit()` (to retain state for the
/// rest of the test binary).
///
/// The pool and its database live for the test binary's lifetime; the
/// shared container is stopped and removed at process exit by the
/// `#[dtor]` below.
///
/// Using this under `DATABASE_URL` override leaks the shared database in
/// the external server — that's acceptable because the database name is a
/// UUID and conflicts are impossible across runs.
pub async fn shared_migrated_pool() -> &'static PgPool {
    SHARED_MIGRATED
        .get_or_init(|| async {
            let shared = shared().await;
            let db_name = format!("meshmon_shared_{}", Uuid::new_v4().simple());

            let admin = PgPool::connect_with(shared.admin_opts.clone())
                .await
                .expect("connect admin");
            admin
                .execute(format!("CREATE DATABASE \"{db_name}\" TEMPLATE template0").as_str())
                .await
                .expect("create shared test database");
            admin.close().await;

            let pool = PgPoolOptions::new()
                .max_connections(SHARED_POOL_MAX_CONNECTIONS)
                .connect_with(shared.admin_opts.clone().database(&db_name))
                .await
                .expect("connect shared test db");
            meshmon_service::db::run_migrations(&pool)
                .await
                .expect("migrate shared test db");
            pool
        })
        .await
}

/// Process-exit cleanup for the shared container.
///
/// Rust statics never run `Drop`, so without this hook the
/// `ContainerAsync` held by [`SHARED`] would leak every `cargo test` run.
/// Synchronous `docker rm -f <id>` via `std::process::Command` sidesteps
/// the need for a Tokio runtime at dtor time (the test harness exits
/// through `libc::exit`, which doesn't keep async executors alive).
///
/// No-op in `DATABASE_URL` override mode (the inner `Option` is `None`).
#[dtor]
fn cleanup_shared_container() {
    let Some(shared) = SHARED.get() else { return };
    let guard = shared
        .container
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(container) = guard.as_ref() else {
        return;
    };
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", container.id()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
