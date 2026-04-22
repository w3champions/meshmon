//! Status-code-focused tests for `POST /api/hostnames/{ip}/refresh`.
//!
//! Exercises just the HTTP surface of the refresh endpoint — the SSE
//! delivery side lives in `hostname_sse_http.rs` and the resolver
//! internals live in `hostname_resolver.rs`. These tests use
//! `oneshot` dispatch against the real router because they only care
//! about status codes, not stream delivery.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::util::ServiceExt;

#[tokio::test]
async fn refresh_accepts_up_to_60_per_minute_then_429s() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.150").await;

    // The limiter permits 60 refreshes per minute. All 60 must return
    // 202 ACCEPTED.
    for i in 0..60 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/hostnames/203.0.113.220/refresh")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("refresh dispatch");
        assert_eq!(
            resp.status(),
            StatusCode::ACCEPTED,
            "request #{i} should succeed, got {}",
            resp.status(),
        );
    }

    // The 61st call within the window must hit the rate limit.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/hostnames/203.0.113.220/refresh")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("refresh dispatch");
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "61st refresh must return 429",
    );
}

#[tokio::test]
async fn refresh_rejects_invalid_ip_literal() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.151").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/hostnames/not-an-ip/refresh")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("refresh dispatch");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "invalid IP literal must return 400",
    );
}

#[tokio::test]
async fn refresh_requires_authentication() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/hostnames/203.0.113.221/refresh")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("refresh dispatch");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "anonymous refresh must return 401",
    );
}
