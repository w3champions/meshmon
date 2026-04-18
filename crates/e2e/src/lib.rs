//! Helpers for meshmon end-to-end tests. The crate is tier-3 in the
//! testing hierarchy (spec 08 §D27): tests under `tests/*.rs` assume
//! the bundled docker-compose stack is **already running** and connect
//! to it as a black-box consumer. They do NOT start or stop
//! infrastructure.
//!
//! Excluded from workspace `default-members`. Run via `cargo e2e`
//! (alias in `.cargo/config.toml`).

use reqwest::blocking::{Client, RequestBuilder, Response};
use std::{sync::OnceLock, time::Duration};

/// Base URL for the meshmon service under test. Defaults to the
/// bundled compose's published port. Override via `MESHMON_E2E_URL` if
/// the stack is reachable elsewhere (e.g. behind a reverse proxy in CI).
pub fn base_url() -> String {
    std::env::var("MESHMON_E2E_URL").unwrap_or_else(|_| "http://localhost:8080".into())
}

/// Admin credentials the preflight + tests authenticate with. Must
/// match the `.env` used to bring up the stack. Defaults align with
/// the CI workflow's deterministic credentials.
pub fn admin_username() -> String {
    std::env::var("MESHMON_E2E_ADMIN_USERNAME").unwrap_or_else(|_| "admin".into())
}
pub fn admin_password() -> String {
    std::env::var("MESHMON_E2E_ADMIN_PASSWORD").unwrap_or_else(|_| "e2e-password".into())
}

/// Preflight: assert the stack is reachable. Every e2e test calls this
/// as its first line. Retries for up to 30s because `docker compose up
/// --wait` only waits for container start, not for meshmon-service to
/// finish migrations and bind its HTTP listener.
pub fn preflight() {
    let url = base_url();
    let client = Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("build reqwest client");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let last_err = loop {
        let err = match client.get(format!("{url}/healthz")).send() {
            Ok(r) if r.status().is_success() => return,
            Ok(r) => format!("HTTP {}", r.status()),
            Err(e) => e.to_string(),
        };
        if std::time::Instant::now() >= deadline {
            break err;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    panic!(
        "meshmon stack not reachable at {url}/healthz after 30s (last error: {last_err}).\n\
         Start it first:\n\
         \n    cd deploy && docker compose up -d --build --wait\n\
         \nthen re-run `cargo e2e`. Override the URL via MESHMON_E2E_URL \
         if the stack is elsewhere."
    );
}

/// HTTP client that reuses cookies across a test. Construct once per
/// test via [`login_as_admin`]; pass to subsequent requests.
pub struct Session {
    client: Client,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        let client = Client::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(10))
            .build()
            .expect("build reqwest client");
        Self { client }
    }

    pub fn get(&self, path: &str) -> RequestBuilder {
        self.client.get(format!("{}{}", base_url(), path))
    }

    pub fn post(&self, path: &str) -> RequestBuilder {
        self.client.post(format!("{}{}", base_url(), path))
    }
}

/// Login as the configured admin. Panics if login fails (the e2e tests
/// are entitled to assume stack-level integrity).
///
/// This performs a fresh login on every call. Use only where tests
/// need to exercise the login endpoint itself; everywhere else call
/// [`shared_admin_session`] so the whole test binary makes a single
/// login, staying inside the service's per-IP login rate limit
/// (3-burst + 1 every 3 min in `crates/service/src/http/auth.rs`).
pub fn login_as_admin() -> Session {
    let session = Session::new();
    let resp: Response = session
        .post("/api/auth/login")
        .json(&serde_json::json!({
            "username": admin_username(),
            "password": admin_password(),
        }))
        .send()
        .expect("POST /api/auth/login");
    assert!(
        resp.status().is_success(),
        "login failed: {} — check MESHMON_E2E_ADMIN_* env vars match the stack's .env",
        resp.status()
    );
    session
}

/// Shared admin [`Session`] cached for the lifetime of the test
/// process. Lazily initializes via [`login_as_admin`] on first call,
/// then hands out a shared reference — subsequent callers reuse the
/// same session cookie. Prefer this over `login_as_admin` in tests
/// that only need an authenticated client.
///
/// Also gates on the proxied upstreams (Grafana + Alertmanager)
/// being ready — `docker compose up --wait` only checks container
/// start, not application bind. Without this, tests that hit
/// `/grafana/*` or `/alertmanager/*` immediately after preflight
/// race the upstream listeners and flake with 502/503.
pub fn shared_admin_session() -> &'static Session {
    static SHARED: OnceLock<Session> = OnceLock::new();
    SHARED.get_or_init(|| {
        let session = login_as_admin();
        wait_for_ok(&session, "/grafana/api/health");
        wait_for_ok(&session, "/alertmanager/-/ready");
        session
    })
}

/// Poll an authenticated endpoint until it returns 2xx or 30s elapse.
/// Used only to gate on proxied upstream readiness.
fn wait_for_ok(session: &Session, path: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let last = loop {
        let err = match session.get(path).send() {
            Ok(r) if r.status().is_success() => return,
            Ok(r) => format!("HTTP {}", r.status()),
            Err(e) => e.to_string(),
        };
        if std::time::Instant::now() >= deadline {
            break err;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    panic!("{path} did not become ready within 30s (last: {last})");
}
