//! End-to-end HTTP tests for the auth flow: login, logout, 401, 429, and
//! cookie flags. All tests drive the real axum router via
//! `tower::ServiceExt::oneshot`.
//!
//! Each test uses a unique client IP inside RFC 5737 TEST-NET-3
//! (`203.0.113.0/24`) so the per-IP rate-limit bucket cannot contaminate
//! other tests. The authoritative allocation list lives in
//! `tests/common/mod.rs` alongside [`common::login_req`] /
//! [`common::login_as_admin`]; update it when you reserve a new octet.
//! Cross-test bucket contamination is already prevented by each test
//! building its own router, so this is defense-in-depth rather than a
//! correctness requirement.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::util::ServiceExt;

mod common;

use common::login_req;

#[tokio::test]
async fn login_with_correct_credentials_returns_200_and_sets_cookie() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(login_req(
            "admin",
            common::AUTH_TEST_PASSWORD,
            "203.0.113.1",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie present")
        .to_str()
        .unwrap();
    assert!(cookie.contains("meshmon_session="), "cookie = {cookie}");
    assert!(cookie.contains("HttpOnly"), "cookie = {cookie}");
    assert!(cookie.contains("Secure"), "cookie = {cookie}");
    assert!(cookie.contains("SameSite=Lax"), "cookie = {cookie}");
}

#[tokio::test]
async fn login_response_body_echoes_username() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(login_req(
            "admin",
            common::AUTH_TEST_PASSWORD,
            "203.0.113.2",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(parsed["username"], "admin");
}

#[tokio::test]
async fn login_with_wrong_password_returns_401() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(login_req("admin", "wrong", "203.0.113.3"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(parsed["error"], "invalid credentials");
}

#[tokio::test]
async fn login_with_unknown_user_returns_401() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(login_req("eve", common::AUTH_TEST_PASSWORD, "203.0.113.4"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_returns_200() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn rate_limit_kicks_in_after_burst() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    // Spec: 5 attempts per 15 min, burst of 3. After `burst` attempts in
    // quick succession from the same IP, the next request hits 429.
    // Use a unique IP per test to avoid cross-test contamination.
    let test_ip = "203.0.113.50";

    for _ in 0..3 {
        let resp = app
            .clone()
            .oneshot(login_req("admin", "wrong", test_ip))
            .await
            .unwrap();
        // First 3 burst requests: 401 (wrong password but accepted by the limiter).
        assert!(
            resp.status() == StatusCode::UNAUTHORIZED
                || resp.status() == StatusCode::TOO_MANY_REQUESTS,
            "unexpected status on burst attempt: {:?}",
            resp.status()
        );
    }
    let limited = app
        .clone()
        .oneshot(login_req("admin", "wrong", test_ip))
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn rate_limit_does_not_leak_between_ips() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    // Burn the bucket for IP A.
    for _ in 0..4 {
        let _ = app
            .clone()
            .oneshot(login_req("admin", "wrong", "203.0.113.60"))
            .await
            .unwrap();
    }
    // IP B should still have a fresh bucket.
    let resp = app
        .clone()
        .oneshot(login_req("admin", "wrong", "203.0.113.61"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn peer_addr_extractor_reads_connect_info_when_trust_disabled() {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_peer_only(pool);
    let app = meshmon_service::http::router(state);

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 80)), 12345);
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("content-type", "application/json")
        // Forged header — the peer-only extractor must ignore it.
        .header("x-forwarded-for", "1.2.3.4")
        .body(Body::from(
            serde_json::json!({
                "username": "admin",
                "password": "wrong"
            })
            .to_string(),
        ))
        .unwrap();
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo(addr));

    let resp = app.oneshot(req).await.unwrap();
    // Not 500 (ConnectInfo IS present) and not 429 (fresh bucket for this peer IP).
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
