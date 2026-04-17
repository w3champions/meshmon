//! SPA fallback integration tests. Exercises the embedded
//! `frontend/dist/` served via `memory-serve`, including the
//! API-prefix guard that prevents SPA hijacking of backend 404s.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use meshmon_service::config::Config;
use meshmon_service::state::AppState;
use sqlx::postgres::PgPool;
use std::sync::Arc;
use tokio::sync::watch;
use tower::util::ServiceExt;

mod common;

async fn make_state(pool: PgPool) -> AppState {
    let cfg = Arc::new(
        Config::from_str(
            r#"
[database]
url = "postgres://ignored@localhost/nope"

[probing]
udp_probe_secret = "hex:0011223344556677"
"#,
            "synthetic.toml",
        )
        .expect("synthetic config"),
    );
    let swap = Arc::new(arc_swap::ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = common::dummy_ingestion(pool.clone());
    let registry = common::dummy_registry(pool.clone());
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        common::test_prometheus_handle().await,
    )
}

#[tokio::test]
async fn spa_deep_link_serves_index_html_with_no_cache() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = make_state(pool).await;
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/agents/brazil-north")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("content-type present")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(ct.starts_with("text/html"), "unexpected ct: {ct}");

    let cc = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .expect("cache-control present")
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(cc, "no-cache, no-store, must-revalidate");

    let body = axum::body::to_bytes(resp.into_body(), 128 * 1024)
        .await
        .unwrap();
    let html = std::str::from_utf8(&body).unwrap();
    assert!(
        html.contains("<html") || html.contains("<!doctype html"),
        "body did not look like HTML: {html:.200}"
    );
}

#[tokio::test]
async fn unknown_api_route_returns_404_not_spa_html() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = make_state(pool).await;
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/this-endpoint-does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    assert!(
        !ct.starts_with("text/html"),
        "SPA fallback hijacked /api path; content-type was {ct:?}"
    );
}

#[tokio::test]
async fn hashed_asset_sets_immutable_cache_and_honors_if_none_match() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = make_state(pool).await;
    let app = meshmon_service::http::router(state);

    // Seeded by build.rs (`prepare_frontend_dist`). Stable contents →
    // stable ETag across test runs.
    let fixture_path = "/assets/meshmon-test-fixture-abcdef12.js";

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(fixture_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.starts_with("text/javascript") || ct.starts_with("application/javascript"),
        "unexpected mime: {ct}"
    );

    assert_eq!(
        resp.headers()
            .get(header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap(),
        "max-age=31536000, immutable"
    );

    let etag = resp
        .headers()
        .get(header::ETAG)
        .expect("etag present")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(!etag.is_empty());

    // Second request with If-None-Match: expect 304 with no body.
    let resp = app
        .oneshot(
            Request::builder()
                .uri(fixture_path)
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    let body = axum::body::to_bytes(resp.into_body(), 128 * 1024)
        .await
        .unwrap();
    assert!(body.is_empty(), "304 must have empty body");
}

#[tokio::test]
async fn backend_routes_survive_spa_fallback_wiring() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = make_state(pool).await;
    state.mark_ready();
    let app = meshmon_service::http::router(state);

    // /healthz must stay a 200 non-HTML response.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    assert!(!ct.starts_with("text/html"), "healthz ct was {ct:?}");

    // /api/openapi.json must return JSON.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("ct")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(ct.starts_with("application/json"), "openapi ct was {ct}");

    // /metrics must return Prometheus text format.
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
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("ct")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(ct.starts_with("text/plain"), "metrics ct was {ct}");
}
