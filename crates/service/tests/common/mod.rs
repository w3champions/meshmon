// Each integration-test binary compiles this module independently; items
// here are consumed by only a subset of binaries (e.g. `http_smoke` uses
// `shared_migrated_pool` but not `acquire`), so we opt out of `dead_code`
// here rather than sprinkling per-item attributes.
#![allow(dead_code)]

//! Integration-test harness for the meshmon service.
//!
//! # Three isolation tiers
//!
//! Pick the entry point that matches what your test actually needs. Going
//! below the listed tier is a contract violation and will flake under
//! parallel execution (nextest).
//!
//! | Entry point                  | Uses shared DB? | Isolation                                | Cost      |
//! |------------------------------|-----------------|------------------------------------------|-----------|
//! | `shared_migrated_pool()`     | yes             | transaction rollback (caller wraps)      | ~ms       |
//! | `acquire(with_timescale)`    | yes             | fresh UUID-named database per test       | ~100 ms   |
//! | `own_container()`            | no              | dedicated TimescaleDB container per test | ~4 s boot |
//!
//! ## When to use which
//!
//! - **`shared_migrated_pool()`** — DML tests. MUST wrap work in
//!   `pool.begin()` / `tx.rollback()`. Anything that survives rollback
//!   (sequence advancement, advisory-lock-at-connection-close, explicit
//!   commits) leaks between concurrent tests.
//! - **`acquire(with_timescale)`** — per-database DDL tests: `CREATE
//!   TABLE`, schema assertions, running migrations, installing
//!   `timescaledb` (extensions are per-database). MUST NOT touch
//!   cluster-wide state.
//! - **`own_container()`** — cluster-wide state. Any of: `CREATE ROLE` /
//!   `ALTER ROLE` / `DROP ROLE`, touching `pg_roles`, replication slots,
//!   tablespaces, cluster-level GUCs, `pg_stat_activity` beyond own
//!   sessions. Pays a ~4 s container startup per test — use sparingly.
//!
//! ## Local dev and CI use `cargo xtask test`
//!
//! Canonical workflow:
//! ```sh
//! cargo xtask test            # spawns a per-invocation TimescaleDB container, runs nextest, tears it down
//! cargo xtask test-db down    # reap any leftover meshmon-test-pg-* containers from crashed runs
//! ```
//! Each `cargo xtask test` invocation owns its own container
//! (`meshmon-test-pg-<uuid>` on a kernel-assigned host port), so two
//! concurrent invocations never collide. Inside one invocation, every
//! nextest test process shares that one container via the inherited
//! `DATABASE_URL` env var — that's where the "process-shared" wording
//! below applies.
//!
//! `cargo test` still works (falls back to the per-binary testcontainers
//! path, unchanged). `cargo nextest run` without `DATABASE_URL` will
//! panic loudly — use xtask.
//!
//! `cargo xtask test` excludes the `xtask` and `meshmon-e2e` packages —
//! they run in separate invocations. Verify xtask's own lifecycle
//! commands with `cargo test -p xtask` (does not need `DATABASE_URL`;
//! spawns its own `meshmon-test-pg-<uuid>` containers as part of the
//! test). Run end-to-end tests with `cargo xtask test-e2e`.
//!
//! # What transaction rollback does NOT cover
//!
//! `shared_migrated_pool()` isolation relies on transactional rollback.
//! These pieces of state survive a rollback — don't depend on them:
//!
//! - **Sequence advancement** (`BIGSERIAL` / `nextval()`). `route_snapshots.id`
//!   will skip values across rolled-back inserts. Assert on the existence
//!   of rows, not on specific id values.
//! - **Advisory locks released at connection close.**
//! - **`TRUNCATE ... RESTART IDENTITY`, `VACUUM`, extension-level state,**
//!   and anything else that implicitly commits.
//!
//! If your test needs a truly clean schema, use [`acquire`] instead.
//! If your test touches cluster-wide state, use [`own_container`] instead.
//!
//! # Example: DDL-owning test
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
//! A failing assertion before `close()` leaks the throwaway database
//! inside the shared container, but db names are UUID-suffixed and the
//! container itself is dropped at process exit — no cross-run harm.
//!
//! # Example: DML-only test (default pattern going forward)
//!
//! ```ignore
//! #[tokio::test]
//! async fn my_dml_test() {
//!     let pool = common::shared_migrated_pool().await;
//!     let mut tx = pool.begin().await.unwrap();
//!     sqlx::query("INSERT INTO agents (id, display_name, ip, \
//!                                      tcp_probe_port, udp_probe_port) \
//!                  VALUES ('a', 'Agent A', '10.0.0.1', 8002, 8005)")
//!         .execute(&mut *tx).await.unwrap();
//!     // ... more work on &mut *tx ...
//!     tx.rollback().await.unwrap();
//! }
//! ```
//!
//! # Example: axum HTTP handler test
//!
//! Most T04+ handler tests will want an `Arc<PgPool>` in `AppState`.
//! `shared_migrated_pool()` returns an owned `PgPool` (a fresh pool
//! against the shared migrated database); wrap it in an `Arc` if your
//! handler expects that — `PgPool` is already `Arc`-wrapped internally,
//! so cloning is cheap:
//!
//! ```ignore
//! #[tokio::test]
//! async fn my_handler_test() {
//!     let pool = std::sync::Arc::new(common::shared_migrated_pool().await.clone());
//!     let app = meshmon_service::http::router(AppState { pool });
//!     // ... axum::test_server invocations, each scoped by a transaction if
//!     //     the handler-under-test allows a pool injection seam.
//! }
//! ```

pub mod grpc_harness;

use async_trait::async_trait;
use ctor::dtor;
use futures::Stream;
use meshmon_service::catalogue::model::Field;
use meshmon_service::config::Config;
use meshmon_service::enrichment::runner::{EnrichmentQueue, Runner};
use meshmon_service::enrichment::{
    EnrichmentError, EnrichmentProvider, EnrichmentResult, FieldValue,
};
use meshmon_service::metrics::Handle as PrometheusHandle;
use meshmon_service::state::AppState;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::Executor;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::{watch, OnceCell};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

static TEST_PROM: OnceCell<PrometheusHandle> = OnceCell::const_new();

/// Process-wide recorder install. `metrics::set_global_recorder`
/// rejects a second call, so every test in the same binary must share
/// one handle.
pub async fn test_prometheus_handle() -> PrometheusHandle {
    TEST_PROM
        .get_or_init(|| async {
            let h = meshmon_service::metrics::install_recorder();
            meshmon_service::metrics::describe_service_metrics();
            h
        })
        .await
        .clone()
}

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
static SHARED_MIGRATED_DB: OnceCell<String> = OnceCell::const_new();

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
///
/// When created via [`own_container`], this struct also owns the container
/// itself and tears it down on `close()`.
pub struct TestDb {
    pub pool: PgPool,
    pub name: String,
    admin_opts: PgConnectOptions,
    /// Present only when this `TestDb` was created by [`own_container`].
    /// The container is stopped and removed inside `close()` in that case.
    owned_container: Option<ContainerAsync<GenericImage>>,
}

impl TestDb {
    /// Construct a `TestDb` that owns its dedicated container. Called only
    /// from [`own_container`].
    fn owned(
        container: ContainerAsync<GenericImage>,
        pool: PgPool,
        admin_opts: PgConnectOptions,
    ) -> Self {
        Self {
            pool,
            name: "postgres".to_string(),
            admin_opts,
            owned_container: Some(container),
        }
    }

    /// Drop the test database. Always safe to call; `DROP DATABASE ...
    /// WITH (FORCE)` terminates any lingering sessions (Postgres 13+).
    ///
    /// When this `TestDb` owns its container (created via [`own_container`]),
    /// the container is also stopped and removed synchronously via
    /// `docker rm -f`.
    pub async fn close(self) {
        let Self {
            pool,
            name,
            admin_opts,
            owned_container,
        } = self;
        pool.close().await;

        if let Some(container) = owned_container {
            // For the owned-container variant, the container holds the
            // entire server — no separate database to drop. The container
            // teardown below handles full cleanup.
            //
            // Dropping `ContainerAsync` in the async context sends a stop
            // signal; we use `docker rm -f` via the dtor pattern to ensure
            // cleanup even if the runtime is shutting down.
            let id = container.id().to_string();
            drop(container);
            let _ = std::process::Command::new("docker")
                .args(["rm", "-f", &id])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        } else {
            let admin = PgPool::connect_with(admin_opts)
                .await
                .expect("connect admin for teardown");
            let _ = admin
                .execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
                .await;
            admin.close().await;
        }
    }
}

/// Panics if we are running under nextest (`NEXTEST=1`) without a
/// `DATABASE_URL` override. Without the override, each nextest test
/// process would fire the module-level `OnceCell` and spawn its own
/// TimescaleDB container — 14-way parallelism saturates the Docker
/// daemon and takes minutes longer than `cargo test`.
///
/// Called from the top of [`shared_migrated_pool`] and [`acquire`].
/// Exposed as `pub` so the dedicated
/// `crates/service/tests/nextest_guard_smoke.rs` binary can assert the
/// panic contract without having to re-import private test helpers.
pub fn guard_nextest_requires_shared_db() {
    if std::env::var("NEXTEST").is_ok() && std::env::var("DATABASE_URL").is_err() {
        panic!(
            "running under nextest without DATABASE_URL is unsupported. \
             Each nextest process would spawn its own TimescaleDB container. \
             Use `cargo xtask test` (auto-provisions a shared DB) or fall \
             back to `cargo test`."
        );
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
    guard_nextest_requires_shared_db();
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
        owned_container: None,
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
pub async fn shared_migrated_pool() -> PgPool {
    guard_nextest_requires_shared_db();
    let shared = shared().await;
    let db_name = SHARED_MIGRATED_DB
        .get_or_init(|| async {
            let db_name = format!("meshmon_shared_{}", Uuid::new_v4().simple());

            let admin = PgPool::connect_with(shared.admin_opts.clone())
                .await
                .expect("connect admin");
            admin
                .execute(format!("CREATE DATABASE \"{db_name}\" TEMPLATE template0").as_str())
                .await
                .expect("create shared test database");
            admin.close().await;

            let init_pool = PgPoolOptions::new()
                .max_connections(4)
                .connect_with(shared.admin_opts.clone().database(&db_name))
                .await
                .expect("connect shared test db for migrations");
            meshmon_service::db::run_migrations(&init_pool)
                .await
                .expect("migrate shared test db");
            init_pool.close().await;

            db_name
        })
        .await;

    // Return a fresh pool per call. The underlying DB is shared (one-time
    // migrated), but each pool's internal tasks live on the caller's
    // runtime — critical for `#[tokio::test]`, which spins up and tears
    // down its own runtime per test. A long-lived `&'static PgPool` would
    // have its tasks die when the first test's runtime drops, breaking
    // subsequent tests.
    PgPoolOptions::new()
        .max_connections(SHARED_POOL_MAX_CONNECTIONS)
        .connect_with(shared.admin_opts.clone().database(db_name))
        .await
        .expect("connect shared test db")
}

/// Dedicated TimescaleDB container for exactly one test.
///
/// Use this ONLY when the test touches cluster-wide state that cannot be
/// contained by a single database: `pg_roles`, `CREATE ROLE`, replication
/// slots, tablespaces, or cluster-wide GUCs. For every other DDL test,
/// [`acquire`] (fresh DB inside the shared server) is correct and faster.
///
/// **Cleanup contract:** callers MUST `.close().await` at the end of the
/// test for deterministic teardown. On panic paths before `close()`, the
/// container is left running and must be reaped by the developer
/// (`docker rm -f <id>`) or CI cleanup. There is no `Drop` impl on
/// [`TestDb`] for the owned-container variant — `ContainerAsync`'s async
/// teardown is unreliable from a synchronous `Drop`, and the
/// [`cleanup_shared_container`] `#[ctor::dtor]` only reaps the shared
/// container (not owned ones). This is why `own_container()` should be
/// used sparingly — panic-leak risk is the cost of cluster-state
/// isolation.
pub async fn own_container() -> TestDb {
    let container = GenericImage::new(TIMESCALEDB_IMAGE, TIMESCALEDB_TAG)
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_DB", "postgres")
        .start()
        .await
        .expect("spawn dedicated TimescaleDB container");

    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container port");
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let opts = PgConnectOptions::from_str(&url).expect("parse private URL");
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(opts.clone())
        .await
        .expect("connect private pool");

    TestDb::owned(container, pool, opts)
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

/// Same PHC hash as the unit tests (`http::auth::tests::TEST_HASH`).
/// Password: `correct horse battery staple`.
pub const AUTH_TEST_HASH: &str =
    "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY";
pub const AUTH_TEST_PASSWORD: &str = "correct horse battery staple";

/// Spawns an ingestion pipeline connected to an unreachable VM URL for
/// handler tests that don't exercise ingestion. The tokio runtime keeps the
/// workers alive until the test process exits; there is no explicit join
/// because the harness does not expose a shutdown hook per-test. The
/// workers are idle unless the test pushes to them, so the resource
/// footprint is a handful of blocked `select!`-ing tasks.
pub fn dummy_ingestion(pool: sqlx::PgPool) -> meshmon_service::ingestion::IngestionPipeline {
    let token = tokio_util::sync::CancellationToken::new();
    let cfg =
        meshmon_service::ingestion::IngestionConfig::default_with_url("http://127.0.0.1:1".into());
    meshmon_service::ingestion::IngestionPipeline::spawn(cfg, pool, token)
}

/// Registry seeded with no agents; the refresh loop is not started, so
/// handler tests see a fixed empty snapshot unless they call
/// `force_refresh()` themselves. Active-window is 5 minutes (matches the
/// config default); refresh-interval is 60 s (unused without a loop).
pub fn dummy_registry(
    pool: sqlx::PgPool,
) -> std::sync::Arc<meshmon_service::registry::AgentRegistry> {
    std::sync::Arc::new(meshmon_service::registry::AgentRegistry::new(
        pool,
        std::time::Duration::from_secs(60),
        std::time::Duration::from_secs(5 * 60),
    ))
}

/// Producer for an enrichment queue whose receiver is immediately
/// dropped. Tests that construct an `AppState` but do not spawn a
/// [`meshmon_service::enrichment::runner::Runner`] use this helper so
/// enqueues silently no-op via the `TrySendError::Closed` branch rather
/// than panicking or blocking.
pub fn test_enrichment_queue(
) -> std::sync::Arc<meshmon_service::enrichment::runner::EnrichmentQueue> {
    let (queue, _rx) = meshmon_service::enrichment::runner::EnrichmentQueue::new(1024);
    std::sync::Arc::new(queue)
}

/// Fixed synthetic UDP probe secret used by every `state_with_*` helper.
/// `[probing].udp_probe_secret` is required at config parse time (T12); the
/// exact bytes don't matter for these tests because no probing is exercised.
pub const TEST_UDP_PROBE_SECRET_TOML: &str = "hex:0011223344556677";

/// Build the triple of hostname fixtures that every `state_with_*`
/// helper threads into `AppState::new`. Uses `StubHostnameBackend` so
/// no DNS is issued and no `cap_net_raw` is needed.
///
/// Kept as a named helper rather than inlining at each call site so a
/// future signature change (e.g. adding a shared `SessionId` default)
/// touches one place.
pub fn test_hostname_fixtures(
    pool: &PgPool,
) -> (
    meshmon_service::hostname::HostnameBroadcaster,
    std::sync::Arc<meshmon_service::hostname::HostnameRefreshLimiter>,
    meshmon_service::hostname::Resolver,
) {
    let backend = StubHostnameBackend::new();
    let broadcaster = meshmon_service::hostname::HostnameBroadcaster::new();
    let limiter = meshmon_service::hostname::HostnameRefreshLimiter::default_production();
    let resolver =
        meshmon_service::hostname::Resolver::new(backend, broadcaster.clone(), pool.clone(), 32);
    (broadcaster, limiter, resolver)
}

/// Construct an `AppState` with the minimum valid config (no auth users,
/// no `[service]` section). Used by tests that exercise unauthenticated
/// or transport-level routes (health, SPA static files) where user setup
/// is irrelevant.
pub async fn state_minimal(pool: PgPool) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@localhost/nope"

[probing]
udp_probe_secret = "{TEST_UDP_PROBE_SECRET_TOML}"
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    let (hb, hl, hr) = test_hostname_fixtures(&pool);
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        test_prometheus_handle().await,
        test_enrichment_queue(),
        hb,
        hl,
        hr,
    )
}

/// Construct an `AppState` with a single `admin` user whose password is
/// [`AUTH_TEST_PASSWORD`]. Uses `trust_forwarded_headers = true` so tests can
/// set a stable client IP via `X-Forwarded-For` without needing to inject a
/// `ConnectInfo` extension per request.
pub async fn state_with_admin(pool: PgPool) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[service]
trust_forwarded_headers = true

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[probing]
udp_probe_secret = "{TEST_UDP_PROBE_SECRET_TOML}"
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    let (hb, hl, hr) = test_hostname_fixtures(&pool);
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        test_prometheus_handle().await,
        test_enrichment_queue(),
        hb,
        hl,
        hr,
    )
}

/// Same as [`state_with_admin`] but with `[service.metrics_auth]`
/// populated so the `/metrics` Basic-auth middleware is active. The
/// scraper credential is `prom` / [`AUTH_TEST_PASSWORD`]; the admin user
/// stays available under the usual `admin` credential — the two auth
/// surfaces do not share identities.
pub async fn state_with_admin_and_metrics_auth(pool: PgPool) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[probing]
udp_probe_secret = "hex:0011223344556677"

[service]
trust_forwarded_headers = true

[service.metrics_auth]
username = "prom"
password_hash = "{AUTH_TEST_HASH}"

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    let (hb, hl, hr) = test_hostname_fixtures(&pool);
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        test_prometheus_handle().await,
        test_enrichment_queue(),
        hb,
        hl,
        hr,
    )
}

/// Same as [`state_with_admin`] but with `upstream.alertmanager_url` set.
/// Use this for alert-proxy tests that need the upstream URL configured.
pub async fn state_with_admin_and_alertmanager(pool: PgPool, alertmanager_url: &str) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[probing]
udp_probe_secret = "hex:0011223344556677"

[service]
trust_forwarded_headers = true

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[upstream]
alertmanager_url = "{alertmanager_url}"
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    let (hb, hl, hr) = test_hostname_fixtures(&pool);
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        test_prometheus_handle().await,
        test_enrichment_queue(),
        hb,
        hl,
        hr,
    )
}

/// Same as [`state_with_admin`] but with `upstream.grafana_url` set.
/// Use this for Grafana-proxy tests that need the upstream URL configured.
pub async fn state_with_admin_and_grafana(pool: PgPool, grafana_url: &str) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[probing]
udp_probe_secret = "hex:0011223344556677"

[service]
trust_forwarded_headers = true

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[upstream]
grafana_url = "{grafana_url}"
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    let (hb, hl, hr) = test_hostname_fixtures(&pool);
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        test_prometheus_handle().await,
        test_enrichment_queue(),
        hb,
        hl,
        hr,
    )
}

/// Same as [`state_with_admin`] but with `upstream.vm_url` set.
/// Use this for metrics-proxy tests that need the VM URL configured.
pub async fn state_with_admin_and_vm(pool: PgPool, vm_url: &str) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[probing]
udp_probe_secret = "hex:0011223344556677"

[service]
trust_forwarded_headers = true

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[upstream]
vm_url = "{vm_url}"
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    let (hb, hl, hr) = test_hostname_fixtures(&pool);
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        test_prometheus_handle().await,
        test_enrichment_queue(),
        hb,
        hl,
        hr,
    )
}

/// Same as [`state_with_admin`] but with `trust_forwarded_headers = false`.
/// Use this when you need to exercise the `PeerAddrKeyExtractor` branch —
/// tests driven via `oneshot` must inject `ConnectInfo<SocketAddr>` into
/// the request extensions manually.
pub async fn state_with_admin_peer_only(pool: PgPool) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[probing]
udp_probe_secret = "{TEST_UDP_PROBE_SECRET_TOML}"
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    let (hb, hl, hr) = test_hostname_fixtures(&pool);
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        test_prometheus_handle().await,
        test_enrichment_queue(),
        hb,
        hl,
        hr,
    )
}

/// Bearer token used by the in-process gRPC harness.
pub const TEST_AGENT_TOKEN: &str = "test-agent-token-0123456789abcdef";

/// `AppState` with the standard test operator, `TEST_AGENT_TOKEN` set, and
/// `trust_forwarded_headers = true` so tests can drive per-request IPs via
/// `x-forwarded-for` without injecting a real `ConnectInfo`. Generous rate
/// limit so the concurrency test doesn't trip the limiter.
pub async fn state_with_agent_token(pool: PgPool) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[service]
trust_forwarded_headers = true

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[agent_api]
shared_token = "{TEST_AGENT_TOKEN}"
rate_limit_per_minute = 600
rate_limit_burst = 300

[probing]
udp_probe_secret = "{TEST_UDP_PROBE_SECRET_TOML}"
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    let (hb, hl, hr) = test_hostname_fixtures(&pool);
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        test_prometheus_handle().await,
        test_enrichment_queue(),
        hb,
        hl,
        hr,
    )
}

// ---------------------------------------------------------------------------
// Auth-flow helpers.
//
// `X-Forwarded-For` IP allocation (RFC 5737 TEST-NET-3, `203.0.113.0/24`) so
// the per-IP rate-limit bucket cannot contaminate neighbouring tests:
//
// | Octet  | Test                                                      |
// |--------|-----------------------------------------------------------|
// | `.1`   | `auth::login_with_correct_credentials_returns_200_…`      |
// | `.2`   | `auth::login_response_body_echoes_username`               |
// | `.3`   | `auth::login_with_wrong_password_returns_401`             |
// | `.4`   | `auth::login_with_unknown_user_returns_401`               |
// | `.50`  | `auth::rate_limit_kicks_in_after_burst`                   |
// | `.60`  | `auth::rate_limit_does_not_leak_between_ips` (burn IP)    |
// | `.61`  | `auth::rate_limit_does_not_leak_between_ips` (fresh IP)   |
// | `.80`  | `auth::peer_addr_extractor_reads_connect_info_…`          |
// | `.100` | `session::session_returns_version_and_username`           |
// | `.44`  | `hostname_sse_http` + `hostname_refresh` — primary login  |
// | `.201` | `hostname_sse_http::sse_events_do_not_leak_between_…`     |
//
// Pick a fresh octet when adding a new test that hits the login endpoint.
// ---------------------------------------------------------------------------

/// Build a JSON-bodied POST request to `/api/auth/login` with the given
/// credentials and `X-Forwarded-For` client IP. Use this when the test
/// asserts the *response* of the login call (e.g. 200, 401, 429,
/// `Set-Cookie` flags). For tests that just need an authenticated cookie
/// to reach a downstream endpoint, reach for [`login_as_admin`] instead.
pub fn login_req(
    username: &str,
    password: &str,
    client_ip: &str,
) -> axum::http::Request<axum::body::Body> {
    let body = serde_json::json!({ "username": username, "password": password });
    axum::http::Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("content-type", "application/json")
        .header("x-forwarded-for", client_ip)
        .body(axum::body::Body::from(body.to_string()))
        .expect("build login request")
}

/// Insert a minimal `agents` row with only the NOT-NULL required columns.
/// Uses `ON CONFLICT (id) DO NOTHING` so it is safe to call multiple times
/// with the same id (e.g., from concurrent tests sharing the migrated pool).
pub async fn insert_agent(pool: &PgPool, id: &str) {
    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, '10.0.0.1', 8002, 8005) ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("insert_agent({id}) failed: {e}"));
}

/// Like [`insert_agent`] but lets the caller pick the stored IP. Use this
/// when a test needs to exercise the peer-IP-vs-registered-IP match logic
/// (e.g. reject-hijack tests for `open_tunnel`).
pub async fn insert_agent_with_ip(pool: &PgPool, id: &str, ip: std::net::IpAddr) {
    let ip_net = sqlx::types::ipnetwork::IpNetwork::from(ip);
    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, $2, 8002, 8005) ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(ip_net)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("insert_agent_with_ip({id}, {ip}) failed: {e}"));
}

/// Seed campaign-kind measurements for an existing campaign.
///
/// For each `(src, dst, rtt_ms, stddev_ms, loss_ratio)`, inserts a
/// `measurements` row + upserts the matching `campaign_pairs` row to
/// point at the new measurement (kind `'campaign'`, resolution
/// `'succeeded'`). The campaign's other knobs (protocol, probe_count)
/// stay at the campaign's catalog defaults — this helper exists to
/// short-circuit the dispatch path for evaluator tests, not to
/// replicate the full settle-writer.
pub async fn seed_measurements(
    pool: &PgPool,
    campaign_id: &str,
    rows: &[(&str, &str, f32, f32, f32)],
) {
    let campaign_uuid: Uuid = campaign_id.parse().expect("campaign_id is a uuid");
    for (src, dst, rtt, stddev, loss) in rows {
        let m_id: i64 = sqlx::query_scalar(
            "INSERT INTO measurements \
                 (source_agent_id, destination_ip, protocol, probe_count, \
                  latency_avg_ms, latency_stddev_ms, loss_ratio, kind) \
             VALUES ($1, $2::inet, 'icmp', 10, $3, $4, $5, 'campaign') \
             RETURNING id",
        )
        .bind(src)
        .bind(dst)
        .bind(rtt)
        .bind(stddev)
        .bind(loss)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("seed_measurements insert measurement ({src} -> {dst}): {e}"));
        sqlx::query(
            "INSERT INTO campaign_pairs \
                 (campaign_id, source_agent_id, destination_ip, \
                  resolution_state, measurement_id, kind) \
             VALUES ($1, $2, $3::inet, 'succeeded', $4, 'campaign') \
             ON CONFLICT (campaign_id, source_agent_id, destination_ip, kind) \
               DO UPDATE SET measurement_id   = EXCLUDED.measurement_id, \
                             resolution_state = 'succeeded'",
        )
        .bind(campaign_uuid)
        .bind(src)
        .bind(dst)
        .bind(m_id)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("seed_measurements upsert campaign_pair ({src} -> {dst}): {e}"));
    }
}

/// Force a campaign into `state='completed'` for tests that need to
/// short-circuit the dispatch loop (e.g. evaluator coverage without
/// running the scheduler). Mirrors the columns
/// [`meshmon_service::campaign::repo::transition_state`] would update
/// on a natural running → completed transition.
pub async fn mark_completed(pool: &PgPool, campaign_id: &str) {
    let id: Uuid = campaign_id.parse().expect("campaign_id is a uuid");
    sqlx::query(
        "UPDATE measurement_campaigns \
             SET state = 'completed', completed_at = now() \
           WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("mark_completed({campaign_id}) failed: {e}"));
}

/// Create a minimal agent + `measurement_campaigns` row for the Raw-tab
/// integration tests. Returns the campaign UUID. The agent id is reused
/// as the agent's display name and its IP is synthesised from the
/// 10.10.10.0/24 range (disjoint from `seed_measurements`).
pub async fn seed_minimal_campaign_for_measurements(pool: &PgPool, agent_id: &str) -> Uuid {
    let campaign_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, '10.10.10.1'::inet, 8002, 8005) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(agent_id)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("seed_minimal_campaign_for_measurements agent ({agent_id}): {e}"));
    sqlx::query(
        "INSERT INTO measurement_campaigns (id, title, protocol, state, created_at) \
         VALUES ($1, 'seed', 'icmp', 'completed', now())",
    )
    .bind(campaign_id)
    .execute(pool)
    .await
    .unwrap_or_else(|e| {
        panic!("seed_minimal_campaign_for_measurements campaign ({campaign_id}): {e}")
    });
    campaign_id
}

/// Insert a measurement + matching settled campaign_pair row for the
/// Raw-tab integration tests. Returns `(pair_id, measurement_id)`.
///
/// Uses explicit casts on the ENUM params (`$3::measurement_kind`) so
/// the runtime-checked `sqlx::query` form still binds correctly.
pub async fn seed_settled_pair(
    pool: &PgPool,
    campaign_id: Uuid,
    source: &str,
    dest: &str,
    kind: &str,
) -> (i64, i64) {
    let measurement_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, \
              measured_at, latency_avg_ms, loss_ratio, kind) \
         VALUES ($1, $2::inet, 'icmp', 10, now(), 25.0, 0.0, $3::measurement_kind) \
         RETURNING id",
    )
    .bind(source)
    .bind(dest)
    .bind(kind)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("seed_settled_pair measurement ({source} -> {dest}, {kind}): {e}"));
    let pair_id: i64 = sqlx::query_scalar(
        "INSERT INTO campaign_pairs \
             (campaign_id, source_agent_id, destination_ip, resolution_state, \
              kind, measurement_id, settled_at) \
         VALUES ($1, $2, $3::inet, 'succeeded', $4::measurement_kind, $5, now()) \
         RETURNING id",
    )
    .bind(campaign_id)
    .bind(source)
    .bind(dest)
    .bind(kind)
    .bind(measurement_id)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("seed_settled_pair pair ({source} -> {dest}, {kind}): {e}"));
    (pair_id, measurement_id)
}

/// Seed a `campaign_evaluations` parent row directly (skipping the
/// evaluator) so paginated-pair_details endpoint tests can shape exact
/// row layouts. Returns the generated evaluation id.
///
/// Mirrors what [`meshmon_service::campaign::evaluation_repo::insert_evaluation`]
/// would write at `/evaluate` time, minus the candidates/pair_details
/// children — those are seeded by [`seed_pair_detail_candidate`] and
/// [`seed_pair_detail_row`].
pub async fn seed_evaluation_row(pool: &PgPool, campaign_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO campaign_evaluations \
             (campaign_id, loss_threshold_ratio, stddev_weight, \
              evaluation_mode, baseline_pair_count, candidates_total, \
              candidates_good, evaluated_at) \
         VALUES ($1, 0.05, 1.0, 'optimization'::evaluation_mode, 0, 0, 0, now()) \
         RETURNING id",
    )
    .bind(campaign_id)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("seed_evaluation_row({campaign_id}): {e}"))
}

/// Seed a `campaign_evaluation_candidates` row directly. The
/// `(evaluation_id, destination_ip)` pair is the FK target for
/// pair_detail rows.
pub async fn seed_pair_detail_candidate(
    pool: &PgPool,
    evaluation_id: Uuid,
    destination_ip: IpAddr,
) {
    let ip_net = sqlx::types::ipnetwork::IpNetwork::from(destination_ip);
    sqlx::query(
        "INSERT INTO campaign_evaluation_candidates \
             (evaluation_id, destination_ip, is_mesh_member, \
              pairs_improved, pairs_total_considered) \
         VALUES ($1, $2::inet, false, 0, 0)",
    )
    .bind(evaluation_id)
    .bind(ip_net)
    .execute(pool)
    .await
    .unwrap_or_else(|e| {
        panic!("seed_pair_detail_candidate({evaluation_id}, {destination_ip}): {e}")
    });
}

/// Inputs for [`seed_pair_detail_row`].
#[derive(Debug, Clone)]
pub struct PairDetailSeed<'a> {
    pub source_agent_id: &'a str,
    pub destination_agent_id: &'a str,
    pub direct_rtt_ms: f32,
    pub direct_stddev_ms: f32,
    pub direct_loss_ratio: f32,
    pub transit_rtt_ms: f32,
    pub transit_stddev_ms: f32,
    pub transit_loss_ratio: f32,
    pub improvement_ms: f32,
    pub qualifies: bool,
}

impl<'a> PairDetailSeed<'a> {
    /// Build a "default-ish" pair_detail seed where transit values are
    /// derived from the improvement (direct_rtt − improvement = transit_rtt).
    /// Tests that need finer control overwrite the relevant fields.
    pub fn baseline(
        source: &'a str,
        destination: &'a str,
        improvement_ms: f32,
        qualifies: bool,
    ) -> Self {
        let direct_rtt_ms = 200.0;
        let transit_rtt_ms = direct_rtt_ms - improvement_ms;
        Self {
            source_agent_id: source,
            destination_agent_id: destination,
            direct_rtt_ms,
            direct_stddev_ms: 5.0,
            direct_loss_ratio: 0.0,
            transit_rtt_ms,
            transit_stddev_ms: 5.0,
            transit_loss_ratio: 0.0,
            improvement_ms,
            qualifies,
        }
    }
}

/// Seed a single `campaign_evaluation_pair_details` row directly. The
/// candidate must already exist (call [`seed_pair_detail_candidate`]
/// first).
pub async fn seed_pair_detail_row(
    pool: &PgPool,
    evaluation_id: Uuid,
    candidate_destination_ip: IpAddr,
    seed: &PairDetailSeed<'_>,
) {
    let ip_net = sqlx::types::ipnetwork::IpNetwork::from(candidate_destination_ip);
    sqlx::query(
        "INSERT INTO campaign_evaluation_pair_details \
             (evaluation_id, candidate_destination_ip, source_agent_id, \
              destination_agent_id, direct_rtt_ms, direct_stddev_ms, \
              direct_loss_ratio, direct_source, transit_rtt_ms, \
              transit_stddev_ms, transit_loss_ratio, improvement_ms, qualifies) \
         VALUES ($1, $2::inet, $3, $4, $5, $6, $7, \
                 'active_probe'::pair_detail_direct_source, \
                 $8, $9, $10, $11, $12)",
    )
    .bind(evaluation_id)
    .bind(ip_net)
    .bind(seed.source_agent_id)
    .bind(seed.destination_agent_id)
    .bind(seed.direct_rtt_ms)
    .bind(seed.direct_stddev_ms)
    .bind(seed.direct_loss_ratio)
    .bind(seed.transit_rtt_ms)
    .bind(seed.transit_stddev_ms)
    .bind(seed.transit_loss_ratio)
    .bind(seed.improvement_ms)
    .bind(seed.qualifies)
    .execute(pool)
    .await
    .unwrap_or_else(|e| {
        panic!(
            "seed_pair_detail_row({evaluation_id}, {candidate_destination_ip}, \
             {src}->{dest}): {e}",
            src = seed.source_agent_id,
            dest = seed.destination_agent_id,
        )
    });
}

/// Insert a dispatched campaign_pair with no joined measurement — models
/// an in-flight detail run for the Raw-tab LEFT-JOIN coverage.
pub async fn seed_pending_pair(
    pool: &PgPool,
    campaign_id: Uuid,
    source: &str,
    dest: &str,
    kind: &str,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO campaign_pairs \
             (campaign_id, source_agent_id, destination_ip, resolution_state, \
              kind, dispatched_at) \
         VALUES ($1, $2, $3::inet, 'dispatched', $4::measurement_kind, now()) \
         RETURNING id",
    )
    .bind(campaign_id)
    .bind(source)
    .bind(dest)
    .bind(kind)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("seed_pending_pair pair ({source} -> {dest}, {kind}): {e}"))
}

/// Response-body byte ceiling for `HttpHarness` helpers. 4 MiB is
/// enough for every catalogue response the integration tests send and
/// receive today; larger payloads should build requests by hand so the
/// limit is an explicit test-level decision rather than silent
/// truncation.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Convenience wrapper around `AppState + router + login cookie` used
/// by the T11 catalogue HTTP tests. Keeps the three smoke tests terse
/// without being a general-purpose fixture (`state_with_admin` +
/// `login_as_admin` remains the canonical path for richer flows).
pub struct HttpHarness {
    /// Axum router with the real production wiring, bound to a
    /// test-scoped `AppState`.
    pub app: axum::Router,
    /// Pre-issued `Set-Cookie` value from a successful admin login.
    /// Callers attach it via the `Cookie` header on every request.
    pub cookie: String,
    /// The same `AppState` baked into `app`, surfaced so tests can
    /// reach the shared Postgres pool (or broker / state helpers added
    /// later).
    pub state: meshmon_service::state::AppState,
    /// Populated only when the harness was started via
    /// [`Self::start_with_providers`]. Tests that hold one of these
    /// also get a real HTTP listener, a `reqwest::Client`, and an
    /// enrichment runner driving the paired receiver; the inner
    /// [`LiveHarness`] owns every task so drop cleanup cancels them.
    live: Option<LiveHarness>,
}

/// Handles owned by a harness started via
/// [`HttpHarness::start_with_providers`]. Exists so the base
/// `HttpHarness::start()` path (oneshot-only, no runner, no listener)
/// keeps paying nothing for the E2E-specific plumbing.
///
/// The struct's fields stay private: every access path goes through
/// [`HttpHarness`] methods so test code never reaches across a shutdown
/// boundary (e.g. hitting the reqwest client after `Drop` has cancelled
/// the server task).
struct LiveHarness {
    /// `127.0.0.1:<assigned>` — resolved from the OS-bound port so
    /// parallel tests never collide on a fixed port.
    addr: SocketAddr,
    /// Long-lived `reqwest::Client`. Reused across every live call to
    /// avoid TLS handshake setup (there is none, but the connection
    /// pool still saves one round-trip per request).
    client: reqwest::Client,
    /// Cancels the `axum::serve` future on drop so the server socket
    /// releases before the next test acquires its port range.
    shutdown: CancellationToken,
    /// Owns the `axum::serve` task. Aborted on drop as a second-line
    /// safety net if graceful shutdown stalls.
    server_task: JoinHandle<()>,
    /// Owns the enrichment runner, when the harness started one.
    /// Aborted on drop to prevent the runner from outliving the test
    /// process and leaking pg connections. `None` when the harness
    /// was spawned without an enrichment runner (e.g. the hostname
    /// resolver variant only needs the HTTP surface).
    runner_task: Option<JoinHandle<()>>,
    /// Per-test throwaway Postgres database. Owned here so `Drop`
    /// closes it synchronously via [`TestDb::close`]; a panic in the
    /// test body will orphan the database inside the shared container,
    /// which is acceptable (container teardown reaps everything) but
    /// not ideal — tests that finish cleanly release their DB name.
    db: Option<TestDb>,
}

impl Drop for LiveHarness {
    fn drop(&mut self) {
        // Order matters: cancel FIRST so the graceful-shutdown branch
        // of `axum::serve` gets a chance to flush in-flight responses
        // before the hard abort fires.
        self.shutdown.cancel();
        self.server_task.abort();
        if let Some(task) = self.runner_task.take() {
            task.abort();
        }
        // Note: no `block_on` DB close here — a synchronous `Drop` can
        // deadlock on a single-threaded runtime. The throwaway DB is
        // reaped when the shared container exits at process end.
        // Callers that want deterministic cleanup can invoke
        // [`HttpHarness::shutdown`] before the harness drops.
        let _ = self.db.take();
    }
}

impl HttpHarness {
    /// Start a harness backed by `state_with_admin` and a cookie for
    /// the default `admin` principal. The client IP used for login is
    /// `203.0.113.42` — a TEST-NET-3 address that is not otherwise
    /// reserved by the auth-test allocation table above.
    pub async fn start() -> Self {
        let pool = shared_migrated_pool().await.clone();
        let state = state_with_admin(pool).await;
        let app = meshmon_service::http::router(state.clone());
        let cookie = login_as_admin(&app, "203.0.113.42").await;
        Self {
            app,
            cookie,
            state,
            live: None,
        }
    }

    /// Same as [`Self::start`] but wires `[upstream.vm_url]` to
    /// `vm_url`. Used by campaign-evaluate tests that need to mock VM
    /// responses before the evaluator runs.
    pub async fn start_with_vm(vm_url: &str) -> Self {
        let pool = shared_migrated_pool().await.clone();
        let state = state_with_admin_and_vm(pool, vm_url).await;
        let app = meshmon_service::http::router(state.clone());
        let cookie = login_as_admin(&app, "203.0.113.42").await;
        Self {
            app,
            cookie,
            state,
            live: None,
        }
    }

    /// Start a harness that binds a real TCP listener, spawns an
    /// enrichment runner against `providers` with a **50 ms** sweep
    /// interval, and issues a session cookie for the default `admin`
    /// principal.
    ///
    /// Unlike [`Self::start`], this variant uses a per-test fresh
    /// Postgres database (via [`acquire`]) so E2E assertions on
    /// "every row in the catalogue" do not race other test binaries
    /// that share the migrated pool.
    ///
    /// The returned harness keeps the server and runner alive for the
    /// duration of its lifetime; dropping it cancels both and releases
    /// the socket.
    pub async fn start_with_providers(providers: Vec<Arc<dyn EnrichmentProvider>>) -> Self {
        let db = acquire(false).await;
        meshmon_service::db::run_migrations(&db.pool)
            .await
            .expect("run migrations for e2e harness");

        // Build the enrichment queue pair directly — we need the
        // receiver to drive the runner. `state_with_admin` always
        // installs a throwaway closed-receiver queue; recreate the
        // AppState by hand so the producer on state matches the
        // receiver the runner drains.
        let (queue, rx) = EnrichmentQueue::new(1024);
        let queue = Arc::new(queue);

        let toml = format!(
            r#"
[database]
url = "postgres://ignored@h/d"

[service]
trust_forwarded_headers = true

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[probing]
udp_probe_secret = "{TEST_UDP_PROBE_SECRET_TOML}"
"#
        );
        let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
        let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
        let (_cfg_tx, cfg_rx) = watch::channel(cfg);
        let ingestion = dummy_ingestion(db.pool.clone());
        let registry = dummy_registry(db.pool.clone());
        let (hb, hl, hr) = test_hostname_fixtures(&db.pool);
        let state = AppState::new(
            swap,
            cfg_rx,
            db.pool.clone(),
            ingestion,
            registry,
            test_prometheus_handle().await,
            queue,
            hb,
            hl,
            hr,
        );
        state.mark_ready();

        // 50 ms sweep — production is 30 s, but tests need tight
        // loops so a missed queue enqueue (unlikely, but possible
        // under CI load) still resolves inside the 5 s deadline.
        let runner = Runner::new(
            db.pool.clone(),
            providers,
            state.catalogue_broker.clone(),
            rx,
            Duration::from_millis(50),
            Arc::clone(&state.facets_cache),
        );
        let runner_task = tokio::spawn(runner.run());

        let app = meshmon_service::http::router(state.clone());
        let cookie = login_as_admin(&app, "203.0.113.43").await;

        // Bind ephemeral port on localhost so parallel tests never
        // collide. `tokio::net::TcpListener::bind(("127.0.0.1", 0))`
        // asks the OS for a free port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind e2e TCP listener");
        let addr = listener.local_addr().expect("resolve local addr");

        let shutdown = CancellationToken::new();
        let server_shutdown = shutdown.clone();
        let server_app = app.clone();
        let server_task = tokio::spawn(async move {
            // Ignore serve result — the only meaningful outcome for
            // tests is "stopped" and the drop handler already knows.
            let _ = axum::serve(listener, server_app)
                .with_graceful_shutdown(async move { server_shutdown.cancelled().await })
                .await;
        });

        let client = reqwest::Client::builder()
            // Keep the client tight so a hanging response surfaces as
            // a test failure rather than a timeout against the harness.
            .timeout(Duration::from_secs(10))
            .build()
            .expect("build reqwest client");

        Self {
            app,
            cookie,
            state,
            live: Some(LiveHarness {
                addr,
                client,
                shutdown,
                server_task,
                runner_task: Some(runner_task),
                db: Some(db),
            }),
        }
    }

    /// Start a harness bound to a test-supplied
    /// [`meshmon_service::hostname::ResolverBackend`] so hostname SSE
    /// and refresh tests can pin the resolver's output without going
    /// through hickory. No enrichment runner is spawned. Uses a
    /// per-test fresh Postgres database so concurrent test binaries
    /// don't see each other's `ip_hostname_cache` rows.
    pub async fn start_with_hostname_resolver(
        backend: Arc<dyn meshmon_service::hostname::ResolverBackend>,
    ) -> Self {
        let db = acquire(false).await;
        meshmon_service::db::run_migrations(&db.pool)
            .await
            .expect("run migrations for hostname resolver harness");

        let toml = format!(
            r#"
[database]
url = "postgres://ignored@h/d"

[service]
trust_forwarded_headers = true

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[probing]
udp_probe_secret = "{TEST_UDP_PROBE_SECRET_TOML}"
"#
        );
        let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
        let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
        let (_cfg_tx, cfg_rx) = watch::channel(cfg);
        let ingestion = dummy_ingestion(db.pool.clone());
        let registry = dummy_registry(db.pool.clone());

        // Build the hostname fixtures from the caller-provided backend
        // so tests control exactly which IPs resolve to which outcomes.
        let broadcaster = meshmon_service::hostname::HostnameBroadcaster::new();
        let limiter = meshmon_service::hostname::HostnameRefreshLimiter::default_production();
        let resolver = meshmon_service::hostname::Resolver::new(
            backend,
            broadcaster.clone(),
            db.pool.clone(),
            32,
        );

        let state = AppState::new(
            swap,
            cfg_rx,
            db.pool.clone(),
            ingestion,
            registry,
            test_prometheus_handle().await,
            test_enrichment_queue(),
            broadcaster,
            limiter,
            resolver,
        );
        state.mark_ready();

        let app = meshmon_service::http::router(state.clone());
        let cookie = login_as_admin(&app, "203.0.113.44").await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind hostname resolver TCP listener");
        let addr = listener.local_addr().expect("resolve local addr");

        let shutdown = CancellationToken::new();
        let server_shutdown = shutdown.clone();
        let server_app = app.clone();
        let server_task = tokio::spawn(async move {
            let _ = axum::serve(listener, server_app)
                .with_graceful_shutdown(async move { server_shutdown.cancelled().await })
                .await;
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("build reqwest client");

        Self {
            app,
            cookie,
            state,
            live: Some(LiveHarness {
                addr,
                client,
                shutdown,
                server_task,
                runner_task: None,
                db: Some(db),
            }),
        }
    }

    /// Base URL (e.g. `http://127.0.0.1:12345`) of the live listener,
    /// when the harness has one. Panics when called on a harness built
    /// via [`Self::start`] since there is no server to hit.
    pub fn base_url(&self) -> String {
        let live = self
            .live
            .as_ref()
            .expect("HttpHarness::base_url requires a live-listener variant");
        format!("http://{}", live.addr)
    }

    /// Shared `reqwest::Client` used by the live harness. Returned as
    /// a reference so callers can pass it to [`subscribe_sse`] without
    /// constructing their own client.
    pub fn client(&self) -> &reqwest::Client {
        &self
            .live
            .as_ref()
            .expect("HttpHarness::client requires a live-listener variant")
            .client
    }

    /// Log in again as the default admin user with a distinct client
    /// IP so tower_sessions issues a separate session id. Returned as
    /// the raw `Set-Cookie` value so SSE / API calls can attach it.
    /// Used by tests that need two independent sessions against the
    /// same harness (e.g. per-session isolation assertions).
    pub async fn login_additional_session(&self, client_ip: &str) -> String {
        login_as_admin(&self.app, client_ip).await
    }

    /// Open a long-lived SSE connection to `path` and return a stream
    /// of parsed JSON payloads. Only usable from a live-listener harness
    /// (`start_with_providers` or `start_with_hostname_resolver`) — the
    /// oneshot path cannot stream.
    ///
    /// Delegates to [`subscribe_sse`] so the connect-and-wrap logic is
    /// shared with other test harnesses that stand up their own axum
    /// server (e.g. the campaigns SSE listener harness).
    pub async fn sse(&self, path: &str) -> SseStream {
        let live = self.live.as_ref().expect(
            "HttpHarness::sse requires a live-listener harness \
             (start_with_providers or start_with_hostname_resolver) — \
             oneshot cannot stream",
        );
        let base_url = format!("http://{}", live.addr);
        subscribe_sse(&live.client, &base_url, path, &self.cookie).await
    }

    /// Fire a `POST` with a JSON body and deserialize the response body
    /// into `T`. Panics on non-200 status — callers use this when they
    /// expect success and want the parsed body; for status-specific
    /// assertions, build the request manually.
    pub async fn post_json<T: for<'de> serde::Deserialize<'de>>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> T {
        use axum::http::{header, Request, StatusCode};
        use tower::util::ServiceExt;

        // Route through the real listener when the harness has one —
        // SSE tests rely on the same server-side state, and mixing
        // `oneshot` with a live server would create two independent
        // `AppState` clones for the same test. That can't happen today
        // (we share `state` across both paths) but the coupling is
        // load-bearing for future wiring and cheaper to preserve now.
        if let Some(live) = &self.live {
            let url = format!("http://{}{path}", live.addr);
            let resp = live
                .client
                .post(&url)
                .header(reqwest::header::COOKIE, &self.cookie)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .send()
                .await
                .unwrap_or_else(|e| panic!("POST {url} dispatch: {e}"));
            let status = resp.status();
            let bytes = resp
                .bytes()
                .await
                .unwrap_or_else(|e| panic!("POST {url} body read: {e}"));
            assert!(
                status.as_u16() == StatusCode::OK.as_u16(),
                "POST {path} expected 200, got {status}; body = {:?}",
                String::from_utf8_lossy(&bytes),
            );
            return serde_json::from_slice::<T>(&bytes)
                .unwrap_or_else(|e| panic!("decode {path} body: {e}; raw = {:?}", bytes));
        }

        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("build POST request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "POST {path} expected 200, got {}",
            resp.status()
        );
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        serde_json::from_slice::<T>(&bytes)
            .unwrap_or_else(|e| panic!("decode {path} body: {e}; raw = {:?}", &bytes))
    }

    /// Fire a `GET` and return the raw status + body string. The raw
    /// surface lets tests assert on both successful shapes and error
    /// shapes (e.g. 404 bodies) without duplicating the cookie wiring.
    pub async fn get(&self, path: &str) -> (axum::http::StatusCode, String) {
        use axum::http::{header, Request};
        use tower::util::ServiceExt;

        let req = Request::builder()
            .method("GET")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .body(axum::body::Body::empty())
            .expect("build GET request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        // Fail loudly on binary responses — the harness only serves
        // JSON / text handlers today, so any non-UTF-8 body is a test
        // bug worth seeing rather than silently replacing with U+FFFD.
        let body = String::from_utf8(bytes.to_vec()).expect("response body must be valid UTF-8");
        (status, body)
    }

    /// Fire a `PATCH` with a JSON body and deserialize the response
    /// body into `T`. Panics on non-200 status — callers use this when
    /// they expect success and want the parsed body; for status-specific
    /// assertions (404, validation errors), build the request manually.
    pub async fn patch_json<T: for<'de> serde::Deserialize<'de>>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> T {
        use axum::http::{header, Request, StatusCode};
        use tower::util::ServiceExt;

        let req = Request::builder()
            .method("PATCH")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("build PATCH request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "PATCH {path} expected 200, got {}",
            resp.status()
        );
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        serde_json::from_slice::<T>(&bytes)
            .unwrap_or_else(|e| panic!("decode {path} body: {e}; raw = {:?}", &bytes))
    }

    /// Fire a body-less `POST` and deserialize the response body into
    /// `T`. Panics on non-200 status — mirrors [`Self::post_json`] for
    /// routes that accept no request body (e.g.
    /// `/api/campaigns/{id}/start`). For status-specific assertions
    /// (e.g. 409 on a second start), use [`Self::post_empty`] and
    /// check the raw status.
    pub async fn post_json_empty<T: for<'de> serde::Deserialize<'de>>(&self, path: &str) -> T {
        use axum::http::{header, Request, StatusCode};
        use tower::util::ServiceExt;

        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .body(axum::body::Body::empty())
            .expect("build POST request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "POST {path} expected 200, got {}",
            resp.status()
        );
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        serde_json::from_slice::<T>(&bytes)
            .unwrap_or_else(|e| panic!("decode {path} body: {e}; raw = {:?}", &bytes))
    }

    /// Fire a body-less `POST` and return the raw status + body string.
    /// Used by tests that assert on 202 responses with no JSON body
    /// (e.g. the re-enrichment endpoints) without needing a content
    /// type or a request payload. Suitable only for routes that do not
    /// extract a request body — no `Content-Type` or `Content-Length`
    /// header is sent.
    pub async fn post_empty(&self, path: &str) -> (axum::http::StatusCode, String) {
        use axum::http::{header, Request};
        use tower::util::ServiceExt;

        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .body(axum::body::Body::empty())
            .expect("build POST request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        // Fail loudly on binary responses — the harness only serves
        // JSON / text handlers today, so any non-UTF-8 body is a test
        // bug worth seeing rather than silently replacing with U+FFFD.
        let body = String::from_utf8(bytes.to_vec()).expect("response body must be valid UTF-8");
        (status, body)
    }

    /// Fire a `PATCH` with a JSON body and return raw status + body.
    /// Use this when asserting on a non-200 response (e.g. 400 on
    /// validation failure) — [`Self::patch_json`] panics on non-200.
    pub async fn patch_raw(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> (axum::http::StatusCode, String) {
        use axum::http::{header, Request};
        use tower::util::ServiceExt;

        let req = Request::builder()
            .method("PATCH")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("build PATCH request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        let body = String::from_utf8(bytes.to_vec()).expect("response body must be valid UTF-8");
        (status, body)
    }

    /// Fire a `GET` and deserialize the response body into `T`. Panics
    /// on non-200 status — callers use this when they expect success
    /// and want the parsed body; for status-specific assertions (404
    /// bodies, etc.) use [`Self::get`] and assert on the raw shape.
    pub async fn get_json<T: for<'de> serde::Deserialize<'de>>(&self, path: &str) -> T {
        use axum::http::{header, Request, StatusCode};
        use tower::util::ServiceExt;

        if let Some(live) = &self.live {
            let url = format!("http://{}{path}", live.addr);
            let resp = live
                .client
                .get(&url)
                .header(reqwest::header::COOKIE, &self.cookie)
                .send()
                .await
                .unwrap_or_else(|e| panic!("GET {url} dispatch: {e}"));
            let status = resp.status();
            let bytes = resp
                .bytes()
                .await
                .unwrap_or_else(|e| panic!("GET {url} body read: {e}"));
            assert!(
                status.as_u16() == StatusCode::OK.as_u16(),
                "GET {path} expected 200, got {status}; body = {:?}",
                String::from_utf8_lossy(&bytes),
            );
            return serde_json::from_slice::<T>(&bytes)
                .unwrap_or_else(|e| panic!("decode {path} body: {e}; raw = {:?}", bytes));
        }

        let req = Request::builder()
            .method("GET")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .body(axum::body::Body::empty())
            .expect("build GET request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "GET {path} expected 200, got {}",
            resp.status()
        );
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        serde_json::from_slice::<T>(&bytes)
            .unwrap_or_else(|e| panic!("decode {path} body: {e}; raw = {:?}", &bytes))
    }

    /// Fire a `POST` with a JSON body, assert the response status equals
    /// `expected_status`, and return the parsed JSON body. Unlike
    /// [`Self::post_json`] which panics on non-200, this is the dedicated
    /// path for tests that deliberately expect a 4xx/5xx envelope and
    /// want to assert on its `error` code.
    pub async fn post_expect_status(
        &self,
        path: &str,
        body: &serde_json::Value,
        expected_status: u16,
    ) -> serde_json::Value {
        use axum::http::{header, Request};
        use tower::util::ServiceExt;

        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("build POST request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        assert_eq!(
            status.as_u16(),
            expected_status,
            "POST {path} expected {expected_status}, got {status}; body = {:?}",
            String::from_utf8_lossy(&bytes),
        );
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap_or_else(|e| panic!("decode {path} body: {e}; raw = {:?}", &bytes))
    }

    /// Fire a `GET`, assert the response status equals `expected_status`,
    /// and return the parsed JSON body. Dedicated path for non-200 error
    /// envelopes — [`Self::get_json`] panics on non-200.
    pub async fn get_expect_status(&self, path: &str, expected_status: u16) -> serde_json::Value {
        use axum::http::{header, Request};
        use tower::util::ServiceExt;

        let req = Request::builder()
            .method("GET")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .body(axum::body::Body::empty())
            .expect("build GET request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        assert_eq!(
            status.as_u16(),
            expected_status,
            "GET {path} expected {expected_status}, got {status}; body = {:?}",
            String::from_utf8_lossy(&bytes),
        );
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap_or_else(|e| panic!("decode {path} body: {e}; raw = {:?}", &bytes))
    }

    /// Fire a `DELETE` and return the raw status + body string. The raw
    /// surface mirrors `get()` so tests can assert on `204 No Content`
    /// bodies (empty) or error shapes without duplicating cookie wiring.
    pub async fn delete(&self, path: &str) -> (axum::http::StatusCode, String) {
        use axum::http::{header, Request};
        use tower::util::ServiceExt;

        let req = Request::builder()
            .method("DELETE")
            .uri(path)
            .header(header::COOKIE, &self.cookie)
            .body(axum::body::Body::empty())
            .expect("build DELETE request");
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot dispatch");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY_BYTES)
            .await
            .expect("collect body bytes");
        // Fail loudly on binary responses — the harness only serves
        // JSON / text handlers today, so any non-UTF-8 body is a test
        // bug worth seeing rather than silently replacing with U+FFFD.
        let body = String::from_utf8(bytes.to_vec()).expect("response body must be valid UTF-8");
        (status, body)
    }
}

/// Drive a successful login as the default `admin` user on `app` and
/// return the `Set-Cookie` value so the caller can attach it to follow-up
/// requests. Panics if the login fails — callers use this as test setup,
/// not as the unit under test.
pub async fn login_as_admin(app: &axum::Router, client_ip: &str) -> String {
    use axum::http::{header, StatusCode};
    use tower::util::ServiceExt;

    let resp = app
        .clone()
        .oneshot(login_req("admin", AUTH_TEST_PASSWORD, client_ip))
        .await
        .expect("login oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "login setup failed (client_ip = {client_ip})"
    );
    resp.headers()
        .get(header::SET_COOKIE)
        .expect("login set a session cookie")
        .to_str()
        .expect("session cookie is valid utf-8")
        .to_string()
}

// ---------------------------------------------------------------------------
// End-to-end helpers: deterministic enrichment providers + SSE parser.
//
// Everything below is consumed only by `catalogue_paste_e2e.rs` today. Kept
// in `common/mod.rs` rather than a standalone module because each test
// binary compiles its own copy of this module and `grpc_harness` already
// establishes the single-file precedent.
// ---------------------------------------------------------------------------

/// Factory namespace for deterministic [`EnrichmentProvider`] chains
/// used by E2E tests.
///
/// The type is zero-sized and exists only to give call sites a readable
/// grouping (`TestProviders::deterministic_city()`) without introducing
/// a free-function name that could collide with production code.
pub struct TestProviders;

impl TestProviders {
    /// Provider chain that always writes `City = "TestCity"` for every
    /// IP. Sufficient to drive [`MergedFields::any_populated`] to
    /// `true`, which transitions the row to `enriched` — the terminal
    /// status the E2E test asserts on.
    pub fn deterministic_city() -> Vec<Arc<dyn EnrichmentProvider>> {
        vec![Arc::new(DeterministicCityProvider)]
    }
}

/// Fixed-output provider used by [`TestProviders::deterministic_city`].
///
/// Private (module-local) so production code can't accidentally depend
/// on this test double. The `id()` string is stable so any future
/// metrics assertion on the E2E run can join on a known label.
struct DeterministicCityProvider;

#[async_trait]
impl EnrichmentProvider for DeterministicCityProvider {
    fn id(&self) -> &'static str {
        "e2e-deterministic-city"
    }

    fn supported(&self) -> &'static [Field] {
        &[Field::City]
    }

    async fn lookup(&self, _ip: IpAddr) -> Result<EnrichmentResult, EnrichmentError> {
        let mut r = EnrichmentResult::default();
        r.fields
            .insert(Field::City, FieldValue::Text("TestCity".to_string()));
        Ok(r)
    }
}

/// Shared SSE subscribe path used by every integration harness that needs
/// a live stream. Opens a long-lived `GET path` against `base_url` with the
/// supplied session cookie, asserts the server answers with a 2xx, and
/// wraps the body bytes in an [`SseStream`].
///
/// `base_url` is the server origin (e.g. `http://127.0.0.1:1234`) with no
/// trailing slash; `path` is the request path (leading slash included).
/// The per-request timeout is bumped to 60 s so legitimately idle SSE
/// streams aren't cut short by the client-level default.
pub async fn subscribe_sse(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    cookie: &str,
) -> SseStream {
    let url = format!("{base_url}{path}");
    let resp = client
        .get(&url)
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        // Override the client-level timeout for the streaming request:
        // SSE connections may legitimately idle for tens of seconds
        // between events.
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .unwrap_or_else(|e| panic!("SSE connect to {url} failed: {e}"));
    assert!(
        resp.status().is_success(),
        "SSE open expected 2xx, got {} at {url}",
        resp.status()
    );
    SseStream::new(resp.bytes_stream())
}

/// Byte-stream-backed Server-Sent Events parser.
///
/// Wraps the bytes stream returned by `reqwest::Response::bytes_stream()`
/// and yields one `serde_json::Value` per `data:` frame. Every other
/// SSE field (`event:`, `id:`, `retry:`, comments) is ignored — the
/// catalogue SSE handler only emits data frames plus keep-alive
/// comments.
///
/// # Framing
///
/// Per SSE spec: events are separated by a blank line (`\n\n` or
/// `\r\n\r\n`). Inside an event, `data:` lines are concatenated with
/// `\n`. This implementation only exercises the single-line case
/// (the service always emits one-line data frames) but respects the
/// spec framing to stay robust.
pub struct SseStream {
    inner: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
    buffer: String,
}

impl SseStream {
    fn new(
        inner: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    ) -> Self {
        Self {
            inner: Box::pin(inner),
            buffer: String::new(),
        }
    }

    /// Extract the next complete event from the internal buffer, if any.
    ///
    /// Returns `Some(Ok(json))` when a `data:` line was parsed,
    /// `Some(Err(_))` when the JSON failed to parse (a test bug — the
    /// server only emits valid JSON), or `None` when no complete event
    /// has arrived yet.
    fn extract_event(
        &mut self,
    ) -> Option<Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>>> {
        loop {
            // Look for SSE frame boundaries in the buffer. Accept
            // both `\n\n` (server's actual output) and `\r\n\r\n`
            // (spec-compliant alternative) so a future transport
            // change can't break the parser silently.
            let (pos, boundary_len) = self
                .buffer
                .find("\n\n")
                .map(|i| (i, 2))
                .or_else(|| self.buffer.find("\r\n\r\n").map(|i| (i, 4)))?;
            let frame: String = self.buffer.drain(..pos + boundary_len).collect();
            // Collect every `data:` line in the frame. Most frames are
            // a single line; the spec allows multi-line values (join
            // with `\n`), so honour that.
            let mut data_lines: Vec<&str> = Vec::new();
            for line in frame.lines() {
                // Skip comments (`:` prefix) and non-data fields.
                if let Some(rest) = line.strip_prefix("data:") {
                    // One optional leading space per spec.
                    let trimmed = rest.strip_prefix(' ').unwrap_or(rest);
                    data_lines.push(trimmed);
                }
            }
            if data_lines.is_empty() {
                // Keep-alive comment or unrelated frame — drop and
                // wait for the next one.
                continue;
            }
            let joined = data_lines.join("\n");
            return Some(
                serde_json::from_str::<serde_json::Value>(&joined)
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) }),
            );
        }
    }
}

impl Stream for SseStream {
    type Item = Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(ev) = self.extract_event() {
                return Poll::Ready(Some(ev));
            }
            match self.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    // Server closed the stream — if a partial frame is
                    // pending, surface it by draining; otherwise end
                    // the stream cleanly.
                    if self.buffer.is_empty() {
                        return Poll::Ready(None);
                    }
                    // Partial trailing frame without a boundary —
                    // treat as end-of-stream rather than erroring,
                    // matching `eventsource-stream`'s behaviour.
                    self.buffer.clear();
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(Box::new(e))));
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    // Server guarantees UTF-8 for SSE frames; a
                    // non-UTF-8 chunk would be a protocol violation.
                    match std::str::from_utf8(&chunk) {
                        Ok(s) => self.buffer.push_str(s),
                        Err(e) => {
                            return Poll::Ready(Some(Err(Box::new(e))));
                        }
                    }
                }
            }
        }
    }
}

// -------- hostname cache-seeding helpers --------

use meshmon_service::hostname::{record_negative, record_positive};

/// Poll `ip_hostname_cache` up to ~2 seconds for a row on `ip`.
///
/// Used by cold-miss tests to observe that the resolver received an
/// `enqueue` call: the stub backend defaults to `NegativeNxDomain`, so
/// a processed enqueue writes a negative cache row we can see here.
pub async fn wait_for_cache_row(pool: &sqlx::PgPool, ip: IpAddr) -> bool {
    for _ in 0..40 {
        let row: Option<(IpAddr,)> =
            sqlx::query_as("SELECT ip FROM ip_hostname_cache WHERE ip = $1 LIMIT 1")
                .bind(ip)
                .fetch_optional(pool)
                .await
                .expect("query ip_hostname_cache");
        if row.is_some() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Seed a positive cache row for `ip` AFTER any handler-driven cold-miss
/// writes have landed, winning the `DISTINCT ON (ip) ORDER BY
/// resolved_at DESC` race against the stub resolver's cold-miss write.
///
/// The stub backend answers unseeded IPs with `NegativeNxDomain`, so
/// if a handler run triggered a cold miss before the seed, the resulting
/// negative row is drained and replaced with the authoritative positive
/// hostname.
pub async fn seed_hostname_positive(pool: &sqlx::PgPool, ip: IpAddr, hostname: &str) {
    for _ in 0..40 {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ip_hostname_cache WHERE ip = $1")
            .bind(ip)
            .fetch_one(pool)
            .await
            .expect("count cache rows");
        if count.0 > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1")
        .bind(ip)
        .execute(pool)
        .await
        .expect("clear cache");
    record_positive(pool, ip, hostname)
        .await
        .expect("seed positive cache");
}

/// Sibling to [`seed_hostname_positive`] — drains any stub-resolver
/// writes for `ip` then inserts the authoritative negative cache row.
pub async fn seed_hostname_negative(pool: &sqlx::PgPool, ip: IpAddr) {
    for _ in 0..40 {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ip_hostname_cache WHERE ip = $1")
            .bind(ip)
            .fetch_one(pool)
            .await
            .expect("count cache rows");
        if count.0 > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1")
        .bind(ip)
        .execute(pool)
        .await
        .expect("clear cache");
    record_negative(pool, ip)
        .await
        .expect("seed negative cache");
}

// -------- hostname test helpers --------

use meshmon_service::hostname::{LookupOutcome, ResolverBackend};

/// Programmable backend for hostname-resolver tests.
///
/// Each IP can map to a fixed `LookupOutcome` with an optional
/// per-call sleep. Unseeded IPs default to `NegativeNxDomain`.
#[derive(Default)]
pub struct StubHostnameBackend {
    routes: Mutex<std::collections::HashMap<IpAddr, (LookupOutcome, Option<Duration>)>>,
    calls: Mutex<std::collections::HashMap<IpAddr, usize>>,
}

impl StubHostnameBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set(&self, ip: IpAddr, outcome: LookupOutcome) {
        self.routes.lock().unwrap().insert(ip, (outcome, None));
    }

    pub fn set_with_delay(&self, ip: IpAddr, outcome: LookupOutcome, delay: Duration) {
        self.routes
            .lock()
            .unwrap()
            .insert(ip, (outcome, Some(delay)));
    }

    pub fn call_count(&self, ip: IpAddr) -> usize {
        self.calls.lock().unwrap().get(&ip).copied().unwrap_or(0)
    }
}

#[async_trait]
impl ResolverBackend for StubHostnameBackend {
    async fn reverse_lookup(&self, ip: IpAddr) -> LookupOutcome {
        let entry = self
            .routes
            .lock()
            .unwrap()
            .get(&ip)
            .cloned()
            .unwrap_or((LookupOutcome::NegativeNxDomain, None));
        *self.calls.lock().unwrap().entry(ip).or_insert(0) += 1;
        if let Some(d) = entry.1 {
            tokio::time::sleep(d).await;
        }
        entry.0
    }
}

/// Create a campaign, seed minimal measurements, mark it completed, then
/// drive `/start` + `/stop` + `/evaluate` so the campaign carries a live
/// `campaign_evaluations` row. Returns the campaign UUID string.
///
/// `mode_str` is the `evaluation_mode` sent to `POST /api/campaigns`
/// (e.g. `"diversity"`, `"optimization"`, `"edge_candidate"`).
/// For `edge_candidate` the request automatically includes a
/// `useful_latency_ms: 80.0` so the new B1 validation passes.
///
/// Agents `t56v-a` / `t56v-b` / `t56v-c` at `198.51.100.{71,72,73}` are
/// inserted once per process (via `INSERT … ON CONFLICT DO NOTHING`), so
/// multiple tests within the same binary can reuse them safely.
///
/// Reused by Tasks B2, E2, F3, H2, H3.
pub async fn create_evaluated_campaign(h: &HttpHarness, mode_str: &str) -> String {
    let a_ip: IpAddr = "198.51.100.71".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.72".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.73".parse().unwrap();
    insert_agent_with_ip(&h.state.pool, "t56v-a", a_ip).await;
    insert_agent_with_ip(&h.state.pool, "t56v-b", b_ip).await;
    insert_agent_with_ip(&h.state.pool, "t56v-c", c_ip).await;

    let mut create_body = serde_json::json!({
        "title": format!("t56-evaluated-{mode_str}"),
        "protocol": "icmp",
        "source_agent_ids": ["t56v-a", "t56v-b", "t56v-c"],
        "destination_ips": [
            "198.51.100.72", "198.51.100.71", "198.51.100.73", "198.51.100.79",
        ],
        "evaluation_mode": mode_str,
        "loss_threshold_ratio": 0.05,
        "stddev_weight": 1.0,
    });

    // edge_candidate requires useful_latency_ms to be present and positive.
    if mode_str == "edge_candidate" {
        create_body["useful_latency_ms"] = serde_json::json!(80.0);
    }

    let campaign: serde_json::Value = h.post_json("/api/campaigns", &create_body).await;
    let campaign_id = campaign["id"].as_str().expect("campaign id").to_string();

    seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // Baselines a↔b, a↔c, b↔c.
            ("t56v-a", "198.51.100.72", 300.0, 5.0, 0.0),
            ("t56v-b", "198.51.100.71", 300.0, 5.0, 0.0),
            ("t56v-a", "198.51.100.73", 300.0, 5.0, 0.0),
            ("t56v-c", "198.51.100.71", 300.0, 5.0, 0.0),
            ("t56v-b", "198.51.100.73", 300.0, 5.0, 0.0),
            ("t56v-c", "198.51.100.72", 300.0, 5.0, 0.0),
            // Transit through X = 198.51.100.79.
            ("t56v-a", "198.51.100.79", 100.0, 5.0, 0.0),
            ("t56v-b", "198.51.100.79", 101.0, 5.0, 0.0),
            ("t56v-c", "198.51.100.79", 102.0, 5.0, 0.0),
        ],
    )
    .await;

    mark_completed(&h.state.pool, &campaign_id).await;

    // Drive /evaluate to produce the evaluation row.
    let _: serde_json::Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    campaign_id
}

/// `ResolverBackend` that panics on first call. Used by the resolver
/// panic-containment integration test.
pub struct PanicHostnameBackend {
    pub panic_on_first_call: std::sync::atomic::AtomicBool,
}

impl PanicHostnameBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            panic_on_first_call: std::sync::atomic::AtomicBool::new(true),
        })
    }
}

#[async_trait]
impl ResolverBackend for PanicHostnameBackend {
    async fn reverse_lookup(&self, _ip: IpAddr) -> LookupOutcome {
        if self
            .panic_on_first_call
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            panic!("synthetic panic for containment test");
        }
        LookupOutcome::Positive("recovered.example.com".into())
    }
}
