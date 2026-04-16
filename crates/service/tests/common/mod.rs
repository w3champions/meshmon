// Each integration-test binary compiles this module independently; items
// here are consumed by only a subset of binaries (e.g. `http_smoke` uses
// `shared_migrated_pool` but not `acquire`), so we opt out of `dead_code`
// here rather than sprinkling per-item attributes.
#![allow(dead_code)]

//! Integration-test harness for the meshmon service.
//!
//! # Design
//!
//! A single `timescale/timescaledb` container is spawned per test binary on
//! first use and shared across every test in that binary. Tests get
//! isolation at one of two granularities:
//!
//! 1. **Fresh database per test** via [`acquire`] — use when the test runs
//!    `CREATE/ALTER/DROP`, installs an extension, or calls
//!    [`meshmon_service::db::run_migrations`] against a clean slate. In
//!    other words: anything DDL.
//! 2. **Shared pre-migrated database + transaction rollback** via
//!    [`shared_migrated_pool`] — the default for everything else. Writers,
//!    readers, HTTP handlers. Each test wraps its work in
//!    `pool.begin().await?` and rolls back for isolation.
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
//! `acquire(true)` against a plain-Postgres `DATABASE_URL` fails at the
//! `CREATE EXTENSION timescaledb` step — that's intentional: point the
//! override at a TimescaleDB-capable server if you need the TS paths.
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
//!     sqlx::query("INSERT INTO agents (id, display_name, ip) \
//!                  VALUES ('a', 'Agent A', '10.0.0.1')")
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

use ctor::dtor;
use meshmon_service::config::Config;
use meshmon_service::state::AppState;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::Executor;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::{watch, OnceCell};
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
pub async fn shared_migrated_pool() -> PgPool {
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

/// Construct an `AppState` with a single `admin` user whose password is
/// [`AUTH_TEST_PASSWORD`]. Uses `trust_forwarded_headers = true` so tests can
/// set a stable client IP via `X-Forwarded-For` without needing to inject a
/// `ConnectInfo` extension per request.
pub fn state_with_admin(pool: PgPool) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[service]
trust_forwarded_headers = true

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
    AppState::new(swap, rx, pool, ingestion, registry)
}

/// Same as [`state_with_admin`] but with `upstream.alertmanager_url` set.
/// Use this for alert-proxy tests that need the upstream URL configured.
pub fn state_with_admin_and_alertmanager(pool: PgPool, alertmanager_url: &str) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

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
    AppState::new(swap, rx, pool, ingestion, registry)
}

/// Same as [`state_with_admin`] but with `upstream.vm_url` set.
/// Use this for metrics-proxy tests that need the VM URL configured.
pub fn state_with_admin_and_vm(pool: PgPool, vm_url: &str) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

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
    AppState::new(swap, rx, pool, ingestion, registry)
}

/// `AppState` with the standard test operator plus `[web]` config for
/// Grafana fields.
pub fn state_with_admin_and_web(
    pool: PgPool,
    grafana_base_url: Option<&str>,
    dashboards: &[(&str, &str)],
) -> AppState {
    let grafana_line = match grafana_base_url {
        Some(url) => format!("grafana_base_url = \"{url}\""),
        None => String::new(),
    };
    let dashboards_lines: Vec<String> = dashboards
        .iter()
        .map(|(k, v)| format!("{k} = \"{v}\""))
        .collect();
    let dashboards_section = if dashboards_lines.is_empty() {
        String::new()
    } else {
        format!("[web.grafana_dashboards]\n{}", dashboards_lines.join("\n"))
    };
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[service]
trust_forwarded_headers = true

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[web]
{grafana_line}

{dashboards_section}
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    AppState::new(swap, rx, pool, ingestion, registry)
}

/// Same as [`state_with_admin`] but with `trust_forwarded_headers = false`.
/// Use this when you need to exercise the `PeerAddrKeyExtractor` branch —
/// tests driven via `oneshot` must inject `ConnectInfo<SocketAddr>` into
/// the request extensions manually.
pub fn state_with_admin_peer_only(pool: PgPool) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

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
    AppState::new(swap, rx, pool, ingestion, registry)
}

/// Bearer token used by the in-process gRPC harness.
pub const TEST_AGENT_TOKEN: &str = "test-agent-token-0123456789abcdef";

/// `AppState` with the standard test operator, `TEST_AGENT_TOKEN` set, and
/// `trust_forwarded_headers = true` so tests can drive per-request IPs via
/// `x-forwarded-for` without injecting a real `ConnectInfo`. Generous rate
/// limit so the concurrency test doesn't trip the limiter.
pub fn state_with_agent_token(pool: PgPool) -> AppState {
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
"#
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = dummy_ingestion(pool.clone());
    let registry = dummy_registry(pool.clone());
    AppState::new(swap, rx, pool, ingestion, registry)
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
// | `.100` | `web_config::web_config_returns_body_with_session`        |
// | `.101` | `web_config::web_config_populates_grafana_fields_from_…`  |
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
