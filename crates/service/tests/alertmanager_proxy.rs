//! Integration tests for the `/alertmanager/*` transparent reverse
//! proxy.
//!
//! Mirrors the shape of `grafana_proxy.rs`: spawn a real
//! `prom/alertmanager` container, wire it up via
//! `state_with_admin_and_alertmanager`, and drive requests through the
//! full axum router. The `with_peer` shim injects
//! `ConnectInfo<SocketAddr>` into each request because
//! `Router::oneshot` does not synthesise one the way
//! `into_make_service_with_connect_info` does at the listener — without
//! it the `/alertmanager/*` middleware panics with
//! `ExtensionRejection::MissingExtension`.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};
use tower::util::ServiceExt;

mod common;

const AM_IMAGE: &str = "prom/alertmanager";
/// Must match (or be <=) `ALERTMANAGER_TAG` in `deploy/versions.env`.
const AM_TAG_FALLBACK: &str = "v0.32.0";

fn am_tag() -> String {
    std::env::var("ALERTMANAGER_TAG").unwrap_or_else(|_| AM_TAG_FALLBACK.to_owned())
}

/// The `/alertmanager/*` middleware declares
/// `ConnectInfo<SocketAddr>` as a required extractor. `oneshot` doesn't
/// synthesise one the way `into_make_service_with_connect_info` does at
/// the listener, so tests must inject it manually or the middleware
/// returns 500 with `ExtensionRejection::MissingExtension`.
fn with_peer(mut req: Request<Body>, ip: [u8; 4]) -> Request<Body> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 40000);
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

async fn spawn_am() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    // Alertmanager v0.32.0 writes startup lines to stderr with the
    // format `level=INFO ... msg="Listening on" address=[::]:9093`. We
    // match the capitalised substring because the log output preserves
    // the original message casing.
    let container = GenericImage::new(AM_IMAGE, &am_tag())
        .with_wait_for(WaitFor::message_on_stderr("Listening on"))
        .with_exposed_port(ContainerPort::Tcp(9093))
        // AM boots in ~1-2s on warm pulls; give a generous budget so a
        // cold pull on CI doesn't trip the default startup timeout.
        .with_startup_timeout(std::time::Duration::from_secs(60))
        .start()
        .await
        .expect("start alertmanager container — is Docker running?");

    let port = container
        .get_host_port_ipv4(9093)
        .await
        .expect("resolve container host port");
    let base = format!("http://127.0.0.1:{port}");
    (container, base)
}

#[tokio::test]
async fn alertmanager_proxy_requires_session() {
    let (_am, base) = spawn_am().await;
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_alertmanager(pool, &base).await;
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(with_peer(
            Request::builder()
                .method("GET")
                .uri("/alertmanager/api/v2/status")
                .body(Body::empty())
                .unwrap(),
            [203, 0, 113, 120],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn alertmanager_proxy_v2_status_returns_am_json() {
    let (_am, base) = spawn_am().await;
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_alertmanager(pool, &base).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.121").await;

    let resp = app
        .oneshot(with_peer(
            Request::builder()
                .method("GET")
                .uri("/alertmanager/api/v2/status")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
            [203, 0, 113, 121],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body.get("versionInfo").is_some(),
        "AM /api/v2/status must contain a `versionInfo` key; body = {body}"
    );
}

#[tokio::test]
async fn alertmanager_proxy_root_serves_spa() {
    let (_am, base) = spawn_am().await;
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_alertmanager(pool, &base).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.122").await;

    let resp = app
        .oneshot(with_peer(
            Request::builder()
                .method("GET")
                .uri("/alertmanager/")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
            [203, 0, 113, 122],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("AM SPA response must set content-type")
        .to_str()
        .expect("content-type header is valid utf-8");
    assert!(
        content_type.contains("html"),
        "AM SPA root must be served as HTML; content-type = {content_type}"
    );
}

#[tokio::test]
async fn alertmanager_proxy_returns_503_when_upstream_unset() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.123").await;

    let resp = app
        .oneshot(with_peer(
            Request::builder()
                .method("GET")
                .uri("/alertmanager/api/v2/status")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
            [203, 0, 113, 123],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}
