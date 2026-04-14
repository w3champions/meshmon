//! End-to-end smoke: spin up a fresh database via `common::acquire`, point a
//! config at it, boot the axum server on port 0, and verify `/readyz` and
//! `/api/openapi.json` respond via a real TCP connection.
//!
//! Not a `main()` test — we drive `meshmon_service::http::router` directly
//! against a `TcpListener` to keep the test fast and to avoid needing a
//! `MESHMON_CONFIG` env var override. The logic mirrors `main.rs` closely
//! enough that regressions in the startup order will still show up.

use arc_swap::ArcSwap;
use meshmon_service::config::Config;
use meshmon_service::state::AppState;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

mod common;

#[tokio::test]
async fn service_boots_and_serves_readyz() {
    let testdb = common::acquire(/*with_timescale=*/ false).await;
    meshmon_service::db::run_migrations(&testdb.pool)
        .await
        .expect("migrate");

    // Synthesize a config — the HTTP layer doesn't read `database.url`, so
    // a placeholder value suffices. `listen_addr` doesn't matter because
    // we bind our own listener on port 0.
    let cfg_text = r#"
[service]
listen_addr = "127.0.0.1:0"

[database]
url = "postgres://ignored"
"#;
    let cfg = Arc::new(Config::from_str(cfg_text, "startup_test.toml").expect("parse"));
    let swap = Arc::new(ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let state = AppState::new(swap, rx, testdb.pool.clone());

    let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let state_for_serve = state.clone();
    let server = tokio::spawn(async move {
        let app = meshmon_service::http::router(state_for_serve);
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { server_shutdown.cancelled().await })
            .await
    });

    // Before ready flag is flipped, /readyz must be 503.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let r = client
        .get(format!("http://{addr}/readyz"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 503);

    state.mark_ready();

    let r = client
        .get(format!("http://{addr}/readyz"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);

    let r = client
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);

    let r = client
        .get(format!("http://{addr}/api/openapi.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let doc: serde_json::Value = r.json().await.unwrap();
    assert_eq!(doc["openapi"], "3.1.0");

    // Shut down and make sure the server task exits.
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server didn't exit within 5s")
        .expect("server task join")
        .expect("server future");

    testdb.close().await;
}
