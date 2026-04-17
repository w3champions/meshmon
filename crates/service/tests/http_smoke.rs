//! HTTP-layer smoke tests using `tower::ServiceExt::oneshot`. No real
//! database is required; the pool is plumbed through but never queried
//! by the endpoints under test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::util::ServiceExt;

mod common;

#[tokio::test]
async fn healthz_always_returns_200() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_minimal(pool).await;
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
    let state = common::state_minimal(pool).await;
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
async fn metrics_returns_prometheus_format_with_uptime() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_minimal(pool).await;
    state.mark_ready();
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
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/plain"));

    let body = axum::body::to_bytes(resp.into_body(), 128 * 1024)
        .await
        .unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(
        text.contains("meshmon_service_uptime_seconds"),
        "body = {text}"
    );
}

#[tokio::test]
async fn openapi_json_is_valid() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_minimal(pool).await;
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

/// Safety net against handler-annotation drift: every T09 user-api endpoint
/// must show up in the runtime-served `/api/openapi.json`. If this test
/// fails after adding a new handler, the `#[utoipa::path]` attribute is
/// missing or the route is not registered on `api_router`, and
/// `frontend/src/api/openapi.gen.json` will be stale too.
#[tokio::test]
async fn openapi_json_contains_user_api_paths() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_minimal(pool).await;
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
    let paths = parsed["paths"].as_object().expect("paths object");
    for p in [
        "/api/agents",
        "/api/agents/{id}",
        "/api/paths/{src}/{tgt}/routes",
        "/api/paths/{src}/{tgt}/routes/latest",
        "/api/paths/{src}/{tgt}/routes/{snapshot_id}",
        "/api/alerts",
        "/api/alerts/{fingerprint}",
        "/api/metrics/query",
        "/api/metrics/query_range",
    ] {
        assert!(paths.contains_key(p), "missing OpenAPI path: {p}");
    }
}
