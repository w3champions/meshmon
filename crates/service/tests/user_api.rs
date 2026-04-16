//! Integration tests for the user-facing agent API endpoints:
//!
//! - `GET /api/agents` — list all agents from the registry snapshot
//!
//! All tests drive the real axum router via `tower::ServiceExt::oneshot`.
//!
//! `X-Forwarded-For` IP allocations for this binary (`.110`–`.149`):
//!
//! | Octet  | Test                                                        |
//! |--------|-------------------------------------------------------------|
//! | `.110` | `agents_list_returns_empty_array_when_registry_is_empty`    |
//! | `.111` | `agents_list_requires_session`                              |

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
