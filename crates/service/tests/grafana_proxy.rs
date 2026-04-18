//! Integration tests for the `/grafana/*` transparent reverse proxy.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};
use tower::util::ServiceExt;

mod common;

const GRAFANA_IMAGE: &str = "grafana/grafana-oss";
/// Must match (or be <=) `GRAFANA_TAG` in `deploy/versions.env`.
const GRAFANA_TAG_FALLBACK: &str = "13.0.1";

fn grafana_tag() -> String {
    std::env::var("GRAFANA_TAG").unwrap_or_else(|_| GRAFANA_TAG_FALLBACK.to_owned())
}

/// The `/grafana/*` middleware declares
/// `ConnectInfo<SocketAddr>` as a required extractor. `oneshot` doesn't
/// synthesise one the way `into_make_service_with_connect_info` does at the
/// listener, so tests must inject it manually or the middleware returns
/// 500 with `ExtensionRejection::MissingExtension`.
fn with_peer(mut req: Request<Body>, ip: [u8; 4]) -> Request<Body> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 40000);
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

async fn spawn_grafana() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new(GRAFANA_IMAGE, &grafana_tag())
        .with_wait_for(WaitFor::message_on_stdout("HTTP Server Listen"))
        .with_exposed_port(ContainerPort::Tcp(3000))
        .with_env_var("GF_AUTH_DISABLE_LOGIN_FORM", "true")
        .with_env_var("GF_AUTH_ANONYMOUS_ENABLED", "false")
        .with_env_var("GF_AUTH_PROXY_ENABLED", "true")
        .with_env_var("GF_AUTH_PROXY_HEADER_NAME", "X-WEBAUTH-USER")
        .with_env_var("GF_AUTH_PROXY_HEADER_PROPERTY", "username")
        .with_env_var("GF_AUTH_PROXY_AUTO_SIGN_UP", "true")
        .with_env_var("GF_AUTH_PROXY_WHITELIST", "")
        .with_env_var("GF_SECURITY_ALLOW_EMBEDDING", "true")
        // Keep Grafana at default log level: the wait condition above greps
        // for "HTTP Server Listen", which Grafana logs at `level=info`.
        // Forcing the log level to `warn` suppresses that line and
        // testcontainers-rs hits its startup timeout. Give the container
        // a generous start budget — warm pulls take ~30-45s to emit the
        // listen message on CI hardware.
        .with_startup_timeout(std::time::Duration::from_secs(120))
        .start()
        .await
        .expect("start grafana-oss container — is Docker running?");

    let port = container
        .get_host_port_ipv4(3000)
        .await
        .expect("resolve container host port");
    let base = format!("http://127.0.0.1:{port}");
    (container, base)
}

#[tokio::test]
async fn grafana_proxy_requires_session() {
    let (_grafana, base) = spawn_grafana().await;
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_grafana(pool, &base).await;
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(with_peer(
            Request::builder()
                .method("GET")
                .uri("/grafana/api/user")
                .body(Body::empty())
                .unwrap(),
            [203, 0, 113, 113],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn grafana_proxy_forwards_webauth_user_header() {
    let (_grafana, base) = spawn_grafana().await;
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_grafana(pool, &base).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.110").await;

    let resp = app
        .oneshot(with_peer(
            Request::builder()
                .method("GET")
                .uri("/grafana/api/user")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
            [203, 0, 113, 110],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["login"], "admin", "body = {body}");
}

#[tokio::test]
async fn grafana_proxy_strips_client_webauth_header() {
    let (_grafana, base) = spawn_grafana().await;
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_grafana(pool, &base).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.111").await;

    let resp = app
        .oneshot(with_peer(
            Request::builder()
                .method("GET")
                .uri("/grafana/api/user")
                .header(header::COOKIE, cookie)
                .header("X-WEBAUTH-USER", "eve")
                .body(Body::empty())
                .unwrap(),
            [203, 0, 113, 111],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["login"], "admin",
        "client-supplied X-WEBAUTH-USER must be ignored; body = {body}"
    );
}

#[tokio::test]
async fn grafana_proxy_returns_503_when_upstream_unset() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.112").await;

    let resp = app
        .oneshot(with_peer(
            Request::builder()
                .method("GET")
                .uri("/grafana/api/user")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
            [203, 0, 113, 112],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}
