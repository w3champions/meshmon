//! Integration tests for the user-facing agent API endpoints:
//!
//! - `GET /api/agents` — list all agents from the registry snapshot
//! - `GET /api/agents/{id}` — single agent detail
//!
//! All tests drive the real axum router via `tower::ServiceExt::oneshot`.
//!
//! `X-Forwarded-For` IP allocations for this binary (`.110`–`.149`):
//!
//! | Octet  | Test                                                        |
//! |--------|-------------------------------------------------------------|
//! | `.110` | `agents_list_returns_empty_array_when_registry_is_empty`    |
//! | `.111` | `agents_list_requires_session`                              |
//! | `.112` | `agent_detail_returns_404_for_unknown_id`                   |
//! | `.113` | `agent_detail_returns_registry_snapshot_fields`             |

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::util::ServiceExt;

mod common;

// ---------------------------------------------------------------------------
// Task 3: GET /api/agents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agents_list_returns_empty_array_when_registry_is_empty() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.110").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents")
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
    assert!(body.is_array(), "expected JSON array, got: {body}");
    assert_eq!(body.as_array().unwrap().len(), 0, "body = {body}");
}

#[tokio::test]
async fn agents_list_requires_session() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Task 4: GET /api/agents/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agent_detail_returns_404_for_unknown_id() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.112").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents/does-not-exist")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn agent_detail_returns_registry_snapshot_fields() {
    let pool = common::shared_migrated_pool().await.clone();

    // Seed an agent row directly in the DB.
    sqlx::query(
        "INSERT INTO agents (id, display_name, location, ip, lat, lon, agent_version)
         VALUES ('brazil-north', 'Fortaleza', 'BR', '170.80.110.90', -3.7, -38.5, 'v0.1.0')",
    )
    .execute(&pool)
    .await
    .expect("seed agent row");

    let state = common::state_with_admin(pool.clone());

    // Force the registry to pick up the seeded row.
    state
        .registry
        .force_refresh()
        .await
        .expect("registry refresh");

    let app = meshmon_service::http::router(state);

    // Login (build request manually so we can reuse `app` for the detail call).
    let cookie = common::login_as_admin(&app, "203.0.113.113").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents/brazil-north")
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

    assert_eq!(body["id"], "brazil-north", "body = {body}");
    assert_eq!(body["display_name"], "Fortaleza", "body = {body}");
    assert_eq!(body["location"], "BR", "body = {body}");
    assert_eq!(body["ip"], "170.80.110.90", "body = {body}");
    assert_eq!(body["lat"], -3.7, "body = {body}");
    assert_eq!(body["lon"], -38.5, "body = {body}");
    assert_eq!(body["agent_version"], "v0.1.0", "body = {body}");
    assert!(body["registered_at"].is_string(), "body = {body}");
    assert!(body["last_seen_at"].is_string(), "body = {body}");

    // Cleanup: remove the seeded row so other tests aren't affected.
    sqlx::query("DELETE FROM agents WHERE id = 'brazil-north'")
        .execute(&pool)
        .await
        .expect("cleanup agent row");
}
