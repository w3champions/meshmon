//! Helpers for meshmon end-to-end tests. The crate is tier-3 in the
//! testing hierarchy (spec 08 §D27): tests under `tests/*.rs` assume
//! the bundled docker-compose stack is **already running** and connect
//! to it as a black-box consumer. They do NOT start or stop
//! infrastructure.
//!
//! Excluded from workspace `default-members`. Run via `cargo e2e`
//! (alias in `.cargo/config.toml`).

use reqwest::blocking::{Client, RequestBuilder, Response};
use std::time::Duration;

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
/// as its first line. Clear failure mode with a pointer at how to
/// recover.
pub fn preflight() {
    let url = base_url();
    let ok = Client::new()
        .get(format!("{url}/healthz"))
        .timeout(Duration::from_secs(2))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    assert!(
        ok,
        "meshmon stack not reachable at {url}/healthz.\n\
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
