//! HTTP-layer smoke tests using `tower::ServiceExt::oneshot`. No real
//! database is required; the pool is plumbed through but never queried
//! by the endpoints under test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use meshmon_service::config::Config;
use meshmon_service::state::AppState;
use sqlx::postgres::PgPool;
use std::sync::Arc;
use tokio::sync::watch;
use tower::util::ServiceExt;

mod common;

fn make_state(pool: PgPool) -> AppState {
    let cfg = Arc::new(
        Config::from_str(
            r#"
[database]
url = "postgres://ignored@localhost/nope"
"#,
            "synthetic.toml",
        )
        .expect("synthetic config"),
    );
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    AppState::new(swap, rx, pool)
}

#[tokio::test]
async fn healthz_always_returns_200() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = make_state(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn readyz_reflects_ready_flag() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = make_state(pool);
    let app = meshmon_service::http::router(state.clone());

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
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    state.mark_ready();
    let resp = app
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

#[tokio::test]
async fn metrics_emits_build_info_line() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = make_state(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("meshmon_service_build_info"), "body = {text}");
    assert!(text.contains("version="), "body = {text}");
}

#[tokio::test]
async fn openapi_json_is_valid() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = make_state(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json parse");
    assert_eq!(parsed["openapi"], "3.1.0");
    assert_eq!(parsed["info"]["title"], "meshmon Service API");
    assert!(parsed["paths"].is_object());
}
