//! Integration tests for `GET /api/session`.
//!
//! The endpoint is the frontend's session probe: unauthenticated
//! callers must see 401 (so the SPA can bounce to `/login`); a valid
//! session sees a JSON body with `version` and `username`.
//!
//! Unlike the old `web-config` endpoint, `session` carries no Grafana
//! or Alertmanager URLs — those services are same-origin now.
//!
//! `X-Forwarded-For` IP allocations for this binary live in
//! `tests/common/mod.rs` alongside the auth-flow helpers.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::util::ServiceExt;

mod common;

#[tokio::test]
async fn session_requires_session() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_returns_version_and_username() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.100").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/session")
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
    // Lock in the slim contract: the body must NOT leak upstream URLs
    // any more — those are same-origin and hardcoded in the frontend.
    assert!(body.get("grafana_base_url").is_none(), "body = {body}");
    assert!(body.get("grafana_dashboards").is_none(), "body = {body}");
    assert!(body.get("alertmanager_base_url").is_none(), "body = {body}");
}
