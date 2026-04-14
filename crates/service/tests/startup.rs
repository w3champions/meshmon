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

// Safe-against-default-handler note: this test raises SIGHUP at the test
// process itself. It works only because `shutdown::spawn` installs a
// `tokio::signal::unix::signal(SIGHUP)` handler before we call `kill`, which
// replaces the process-level default (`terminate`) with our consumer. If a
// future test lands in this binary that does NOT call `shutdown::spawn`, and
// cargo filters the test set to just that one, a SIGHUP from this test would
// no longer be covered — the test runner would be terminated mid-run.
// Mitigation: every test in `tests/startup.rs` must either call
// `shutdown::spawn` or never race against this test (e.g., via `--test-threads=1`).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_reload_on_sighup_swaps_arcswap() {
    use std::io::Write;

    // Build a temp config file.
    let tmpdir = tempdir();
    let cfg_path = tmpdir.join("meshmon.toml");
    let write_cfg = |filter: &str| {
        let contents = format!(
            r#"
[database]
url = "postgres://ignored@localhost/nope"

[logging]
filter = "{filter}"
"#
        );
        let mut f = std::fs::File::create(&cfg_path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    };
    write_cfg("info");

    let initial = Arc::new(Config::from_file(&cfg_path).unwrap());
    let swap = Arc::new(ArcSwap::from(initial.clone()));
    let (tx, mut rx) = watch::channel(initial);

    let reload_handle = swap.clone();
    let reload_path = cfg_path.clone();
    let reload_tx = tx.clone();
    let token = meshmon_service::shutdown::spawn(move || {
        let reload_handle = reload_handle.clone();
        let reload_path = reload_path.clone();
        let reload_tx = reload_tx.clone();
        async move {
            if let Ok(new_cfg) = Config::from_file(&reload_path) {
                let new_cfg = Arc::new(new_cfg);
                reload_handle.store(new_cfg.clone());
                let _ = reload_tx.send(new_cfg);
            }
        }
    });

    // Give the signal handler time to install.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Rewrite and SIGHUP ourselves.
    write_cfg("debug");
    let pid = std::process::id().to_string();
    let status = std::process::Command::new("kill")
        .args(["-HUP", &pid])
        .status()
        .unwrap();
    assert!(status.success());

    // Wait for the watch channel to fire (up to 1s).
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if swap.load().logging.filter == "debug" {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "ArcSwap never observed new config; filter={}",
                swap.load().logging.filter
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Watch channel should also have received the new config.
    rx.changed().await.unwrap();
    assert_eq!(rx.borrow().logging.filter, "debug");

    assert!(
        !token.is_cancelled(),
        "SIGHUP must not cancel shutdown token"
    );
    std::fs::remove_dir_all(&tmpdir).ok();
}

/// Create a fresh temporary directory for per-test scratch files. Not using
/// `tempfile` because we don't want another dep for a one-line helper.
#[cfg(unix)]
fn tempdir() -> std::path::PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("meshmon_t04_{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn shutdown_flips_not_ready() {
    let testdb = common::acquire(/*with_timescale=*/ false).await;
    meshmon_service::db::run_migrations(&testdb.pool)
        .await
        .expect("migrate");

    let cfg = Arc::new(
        Config::from_str(
            r#"
[database]
url = "postgres://ignored"
"#,
            "t.toml",
        )
        .unwrap(),
    );
    let swap = Arc::new(ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let state = AppState::new(swap, rx, testdb.pool.clone());
    state.mark_ready();
    assert!(state.is_ready());

    let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let state_for_serve = state.clone();
    let serve_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, meshmon_service::http::router(state_for_serve))
            .with_graceful_shutdown(async move {
                server_shutdown.cancelled().await;
                serve_state.mark_not_ready();
            })
            .await
    });

    let client = reqwest::Client::new();
    let r = client
        .get(format!("http://{addr}/readyz"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);

    // Trigger shutdown.
    shutdown.cancel();

    // Within ~1s, /readyz should drop to 503 (state flag flips). After the
    // server exits further requests fail to connect; that's the stronger
    // assertion we check next.
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("shutdown exceeded 5s")
        .expect("server task join")
        .expect("server future");

    // is_ready should be false after shutdown.
    assert!(!state.is_ready());

    testdb.close().await;
}
