//! End-to-end test: the bundled meshmon docker-compose stack boots,
//! serves the meshmon-service surface, and wires Grafana and
//! Alertmanager through the authenticated reverse proxies.
//!
//! Runs via `cargo e2e`. Requires the stack to be already running
//! (see `crates/e2e/src/lib.rs::preflight`).

use meshmon_e2e::{login_as_admin, preflight, shared_admin_session, Session};
use reqwest::blocking::Client;
use std::time::Duration;

#[test]
fn health_and_readiness_are_green() {
    preflight();
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let base = meshmon_e2e::base_url();
    for path in ["/healthz", "/readyz"] {
        let resp = client
            .get(format!("{base}{path}"))
            .send()
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));
        assert!(
            resp.status().is_success(),
            "{path} returned {}",
            resp.status()
        );
    }
}

#[test]
fn login_issues_a_session_cookie() {
    preflight();
    let session: Session = login_as_admin();
    // Hit an authenticated endpoint to prove the cookie is attached.
    let resp = session
        .get("/api/session")
        .send()
        .expect("GET /api/session");
    assert!(
        resp.status().is_success(),
        "/api/session returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().expect("parse /api/session JSON");
    assert_eq!(
        body.get("username").and_then(|v| v.as_str()),
        Some(meshmon_e2e::admin_username().as_str())
    );
}

#[test]
fn grafana_proxy_injects_webauth_user() {
    preflight();
    let session = shared_admin_session();
    // /grafana/api/health is a Grafana endpoint that returns JSON
    // indicating DB health. auth.proxy mode only lets the request
    // through when X-WEBAUTH-USER is injected by meshmon-service; the
    // 200 response here proves the handoff works.
    let resp = session.get("/grafana/api/health").send().unwrap();
    assert!(
        resp.status().is_success(),
        "/grafana/api/health returned {} — Grafana auth.proxy handoff failed",
        resp.status()
    );
    let body: serde_json::Value = resp.json().expect("parse /grafana/api/health JSON");
    assert_eq!(
        body.get("database").and_then(|v| v.as_str()),
        Some("ok"),
        "Grafana database not healthy: {body}"
    );
}

#[test]
fn alertmanager_proxy_serves_status() {
    preflight();
    let session = shared_admin_session();
    let resp = session.get("/alertmanager/api/v2/status").send().unwrap();
    assert!(
        resp.status().is_success(),
        "/alertmanager/api/v2/status returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().expect("parse AM status JSON");
    assert!(
        body.get("versionInfo").is_some(),
        "AM status body has no versionInfo: {body}"
    );
}

#[test]
fn dashboards_are_provisioned() {
    preflight();
    let session = shared_admin_session();
    let resp = session.get("/grafana/api/search").send().unwrap();
    assert!(
        resp.status().is_success(),
        "/grafana/api/search returned {}",
        resp.status()
    );
    let list: Vec<serde_json::Value> = resp.json().expect("parse Grafana search JSON");
    let uids: Vec<String> = list
        .iter()
        .filter_map(|v| v.get("uid").and_then(|u| u.as_str()).map(String::from))
        .collect();
    for expected in ["meshmon-path", "meshmon-overview", "meshmon-agent"] {
        assert!(
            uids.iter().any(|u| u == expected),
            "dashboard uid {expected} missing from Grafana search; got {uids:?}"
        );
    }
}

#[test]
fn unauthenticated_grafana_request_is_rejected() {
    preflight();
    // A raw client with no session cookie should get a 401 on /grafana/*.
    let resp = Client::new()
        .get(format!("{}/grafana/api/health", meshmon_e2e::base_url()))
        .timeout(Duration::from_secs(5))
        .send()
        .unwrap();
    assert!(
        resp.status().as_u16() == 401 || resp.status().as_u16() == 302,
        "unauthenticated /grafana/api/health returned {}; expected 401 or 302",
        resp.status()
    );
}

#[test]
fn alertmanager_route_prefix_is_configured() {
    preflight();
    // Access AM's /-/ready through the proxy. With --web.route-prefix=/alertmanager
    // the endpoint lives at /alertmanager/-/ready; a plain /-/ready
    // would return 404.
    let session = shared_admin_session();
    let resp = session.get("/alertmanager/-/ready").send().unwrap();
    assert!(
        resp.status().is_success(),
        "/alertmanager/-/ready returned {}; is --web.route-prefix=/alertmanager set?",
        resp.status()
    );
}
