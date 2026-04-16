//! Integration tests for the T10 `/metrics` exposition.
//!
//! Covers:
//! - baseline format + uptime + HELP lines;
//! - HTTP request counter (axum-prometheus layer) increments after real
//!   requests reach the router;
//! - registry-derived per-agent gauges appear at scrape time after an
//!   agent registers + the registry is force-refreshed;
//! - the Basic-auth gate guards `/metrics` when `[service.metrics_auth]`
//!   is configured, but `/healthz` / `/readyz` remain ungated so k8s
//!   probes are not affected.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use meshmon_protocol::RegisterRequest;
use std::net::IpAddr;
use tower::util::ServiceExt;

mod common;

/// Drive a GET /metrics through the router and return the response body
/// as a string. Fails loudly if the status is not 200 — every test in
/// this file that calls this expects an unauthenticated scrape to
/// succeed.
async fn scrape(app: &axum::Router) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

#[tokio::test]
async fn metrics_exposes_uptime_and_describes_known_metrics() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let body = scrape(&app).await;
    assert!(
        body.contains("# HELP meshmon_service_uptime_seconds"),
        "missing uptime HELP in:\n{body}"
    );
    assert!(
        body.contains("# TYPE meshmon_service_uptime_seconds gauge"),
        "missing uptime TYPE in:\n{body}"
    );
    // The handler refreshes the uptime gauge at scrape time, so a
    // sample line must be present regardless of elapsed wall clock.
    assert!(
        body.contains("meshmon_service_uptime_seconds "),
        "missing uptime sample line in:\n{body}"
    );
}

#[tokio::test]
async fn metrics_counts_http_requests_via_axum_prometheus() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    state.mark_ready();
    let app = meshmon_service::http::router(state);

    // Fire several requests against a route with a stable axum-router
    // template (`/readyz`) so axum-prometheus emits a deterministic
    // `endpoint` label.
    for _ in 0..3 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    let body = scrape(&app).await;
    // axum-prometheus emits labels `method`, `endpoint`, `status`. Label
    // order inside `{...}` is not guaranteed by the exporter, so check
    // for presence of each label value on a single counter line.
    let has_readyz_counter = body
        .lines()
        .filter(|l| l.starts_with("meshmon_service_http_requests_total"))
        .any(|l| l.contains(r#"endpoint="/readyz""#) && l.contains(r#"status="200""#));
    assert!(
        has_readyz_counter,
        "expected meshmon_service_http_requests_total line with endpoint=/readyz and status=200 in:\n{body}"
    );
}

#[tokio::test]
async fn metrics_exposes_registered_agent_info_and_last_seen() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;

    // NOTE: this test mutates the shared migrated pool. The `register`
    // call below commits the `obs-test-agent` row and it persists for
    // the lifetime of the test binary. None of the current tests in
    // this binary assert on agent counts, so the contamination is
    // latent. If a future test here needs clean isolation, switch its
    // pool to `common::acquire(true)` (fresh DB per test) or add an
    // explicit `DELETE FROM agents WHERE id = 'obs-test-agent'` cleanup
    // step.
    let mut client =
        common::grpc_harness::in_process_agent_client(state.clone(), IpAddr::from([10, 99, 0, 1]))
            .await;
    client
        .register(RegisterRequest {
            id: "obs-test-agent".into(),
            display_name: "Obs Test".into(),
            location: String::new(),
            ip: vec![10, 99, 0, 1].into(),
            lat: 0.0,
            lon: 0.0,
            agent_version: "0.42.0".into(),
        })
        .await
        .expect("register");
    // Register already triggers a refresh in production, but being
    // explicit avoids relying on that side effect in a test about the
    // scrape surface.
    state.registry.force_refresh().await.expect("force refresh");

    let app = meshmon_service::http::router(state);
    let body = scrape(&app).await;

    assert!(
        body.contains(r#"meshmon_agent_info{source="obs-test-agent",agent_version="0.42.0"} 1"#),
        "missing agent_info line in:\n{body}"
    );

    // Last-seen gauge: value is the agent's `last_seen_at` Unix seconds.
    // Use the register timestamp as a lower bound — 2023-11-14T22:13:20Z.
    let ts_line = body
        .lines()
        .find(|l| l.starts_with(r#"meshmon_agent_last_seen_seconds{source="obs-test-agent""#))
        .unwrap_or_else(|| panic!("missing last_seen line in:\n{body}"));
    let ts: i64 = ts_line
        .rsplit(' ')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("could not parse timestamp from: {ts_line}"));
    assert!(ts > 1_700_000_000, "timestamp too small: {ts}");
}

#[tokio::test]
async fn metrics_requires_basic_auth_when_configured() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_metrics_auth(pool).await;
    let app = meshmon_service::http::router(state);

    // No header → 401 + a parseable `Basic` challenge so Prometheus
    // retries with creds.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        resp.headers()
            .get(header::WWW_AUTHENTICATE)
            .is_some_and(|v| v.to_str().unwrap().starts_with("Basic")),
        "missing WWW-Authenticate Basic challenge"
    );

    // Correct creds — note the `prom` username from the helper's
    // `[service.metrics_auth]` block, not the `admin` operator user.
    let b64 = STANDARD.encode(format!("prom:{}", common::AUTH_TEST_PASSWORD));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header(header::AUTHORIZATION, format!("Basic {b64}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn healthz_and_readyz_remain_ungated_when_metrics_auth_configured() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_metrics_auth(pool).await;
    state.mark_ready();
    let app = meshmon_service::http::router(state);

    for path in ["/healthz", "/readyz"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "expected {path} ungated even with [service.metrics_auth] set"
        );
    }
}
