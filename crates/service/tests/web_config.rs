//! Integration tests for `GET /api/web-config`.
//!
//! The endpoint is the frontend's session probe: unauthenticated callers
//! must see 401 (so the SPA can bounce them to `/login`), while
//! authenticated callers must see a JSON payload with at least a
//! `version` string and a `grafana_dashboards` object. When the `[web]`
//! config section populates `grafana_base_url` and `grafana_dashboards`,
//! the endpoint surfaces those values; absent Grafana config yields an
//! empty dashboards map and omits `grafana_base_url` from the JSON body.
//!
//! `X-Forwarded-For` IP allocations for this binary live in
//! `tests/common/mod.rs` alongside the auth-flow helpers.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::util::ServiceExt;

mod common;

#[tokio::test]
async fn web_config_requires_session() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/web-config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn web_config_returns_body_with_session() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.100").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/web-config")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["version"].is_string(), "body = {body}");
    assert_eq!(body["username"], "admin", "body = {body}");
    assert!(body["grafana_dashboards"].is_object(), "body = {body}");
    // MVP contract: `grafana_base_url` is absent from the JSON body when
    // Grafana is not configured (serialized with `skip_serializing_if =
    // "Option::is_none"`). Lock this in so a later change can't silently
    // start emitting `"grafana_base_url": null`.
    assert!(body.get("grafana_base_url").is_none(), "body = {body}");
}

#[tokio::test]
async fn web_config_populates_grafana_fields_from_config() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_web(
        pool,
        Some("https://grafana.example.com"),
        &[
            ("overview", "meshmon-overview"),
            ("path-detail", "meshmon-path"),
        ],
    )
    .await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.101").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/web-config")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["grafana_base_url"], "https://grafana.example.com");
    let dashboards = body["grafana_dashboards"].as_object().unwrap();
    assert_eq!(dashboards["overview"], "meshmon-overview");
    assert_eq!(dashboards["path-detail"], "meshmon-path");
}
