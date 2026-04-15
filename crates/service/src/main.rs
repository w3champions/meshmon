//! `meshmon-service` binary entry point.
//!
//! Startup order (spec 03 §Startup):
//! 1. Load config (`$MESHMON_CONFIG`, default `/etc/meshmon/meshmon.toml`).
//! 2. Initialize tracing subscriber.
//! 3. Connect Postgres + run migrations.
//! 4. Best-effort reachability checks for VictoriaMetrics and Alertmanager
//!    (warn on failure — those upstreams may not exist yet during rollout).
//! 5. Bind HTTP listener.
//! 6. Mark service ready.
//! 7. Run the axum server with graceful shutdown, driven by SIGTERM/SIGINT.

use anyhow::Context;
use arc_swap::ArcSwap;
use meshmon_service::config::{Config, DEFAULT_CONFIG_PATH};
use meshmon_service::state::AppState;
use meshmon_service::{db, http, logging, shutdown};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{info, warn};

fn main() -> anyhow::Result<()> {
    // The tokio runtime is created here (not via `#[tokio::main]`) so that
    // panics in runtime setup surface as normal process exits, not as the
    // tokio-runtime panic handler's abort.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(run())
}

async fn run() -> anyhow::Result<()> {
    // --- Step 1: Load config ---
    let config_path = std::env::var("MESHMON_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH));
    let initial_config = Arc::new(Config::from_file(&config_path)?);
    let config_handle = Arc::new(ArcSwap::from(initial_config.clone()));
    let (config_tx, config_rx) = watch::channel(initial_config.clone());

    // --- Step 2: Logging ---
    logging::init(&initial_config.logging);
    info!(
        config_path = %config_path.display(),
        version = env!("CARGO_PKG_VERSION"),
        "meshmon-service starting"
    );

    // --- Step 3: Postgres + migrations ---
    let pool = db::connect(initial_config.database.url())
        .await
        .context("connect postgres")?;
    db::run_migrations(&pool).await.context("run migrations")?;
    info!("database migrations applied");

    // --- Step 4: Upstream reachability (warn on failure) ---
    if let Some(url) = &initial_config.upstream.vm_url {
        if let Err(e) = probe(url, "VictoriaMetrics").await {
            warn!(error = %e, url = %url, "VictoriaMetrics unreachable at startup");
        }
    }
    if let Some(url) = &initial_config.upstream.alertmanager_url {
        if let Err(e) = probe(url, "Alertmanager").await {
            warn!(error = %e, url = %url, "Alertmanager unreachable at startup");
        }
    }

    // --- Step 5: Bind HTTP listener ---
    let listen_addr: SocketAddr = initial_config.service.listen_addr;
    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("bind HTTP listener on {listen_addr}"))?;
    let local_addr = listener.local_addr()?;
    info!(addr = %local_addr, "HTTP listener bound");

    // --- Shutdown coordination ---
    // on_reload: re-read the config file and swap the ArcSwap. Detached by
    // shutdown.rs to keep the signal loop responsive, so rapid SIGHUPs could
    // otherwise race. A tokio Mutex serializes the spawned tasks — each
    // acquires the guard in FIFO order, so the most recent file read is the
    // last to land in ArcSwap.
    let reload_path = config_path.clone();
    let reload_handle = config_handle.clone();
    let reload_tx = config_tx.clone();
    let reload_lock = Arc::new(tokio::sync::Mutex::new(()));
    let shutdown_token = shutdown::spawn(move || {
        let reload_path = reload_path.clone();
        let reload_handle = reload_handle.clone();
        let reload_tx = reload_tx.clone();
        let reload_lock = reload_lock.clone();
        async move {
            let _guard = reload_lock.lock().await;
            match Config::from_file(&reload_path) {
                Ok(new_cfg) => {
                    let new_cfg = Arc::new(new_cfg);
                    reload_handle.store(new_cfg.clone());
                    let _ = reload_tx.send(new_cfg);
                    info!("config reloaded");
                }
                Err(e) => {
                    warn!(error = %e, "config reload failed; keeping previous config");
                }
            }
        }
    });

    // --- Step 6: Build state, mark ready ---
    // NOTE: the VM URL is read once at startup. Changes to `upstream.vm_url`
    // via `SIGHUP` config reload take effect only after a service restart.
    // Wiring the pipeline to observe `config_handle`/`config_rx` is a
    // follow-up (tracked for the T10/exporter work).
    let vm_url_for_ingestion = initial_config.upstream.vm_url.clone().unwrap_or_else(|| {
        warn!("no upstream.vm_url configured; ingestion will fail to write to VM");
        "http://meshmon-vm:8428".to_string()
    });
    let ingestion_cfg = meshmon_service::ingestion::IngestionConfig::default_with_url(format!(
        "{}/api/v1/write",
        vm_url_for_ingestion.trim_end_matches('/')
    ));
    let ingestion = meshmon_service::ingestion::IngestionPipeline::spawn(
        ingestion_cfg,
        pool.clone(),
        shutdown_token.clone(),
    );

    let state = AppState::new(config_handle, config_rx, pool, ingestion.clone());
    state.mark_ready();
    let app = http::router(state.clone());

    // --- Step 7: Serve with a bounded drain ---
    let deadline = state.config().service.shutdown_deadline;

    let graceful_token = shutdown_token.clone();
    let graceful_state = state.clone();
    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        graceful_token.cancelled().await;
        graceful_state.mark_not_ready();
        info!("HTTP server draining");
    });

    let deadline_token = shutdown_token.clone();
    let deadline_timer = async move {
        deadline_token.cancelled().await;
        tokio::time::sleep(deadline).await;
    };

    tokio::select! {
        result = serve => result.context("HTTP server")?,
        _ = deadline_timer => {
            warn!(
                deadline_ms = deadline.as_millis() as u64,
                "HTTP server did not drain within shutdown_deadline; aborting in-flight connections"
            );
        }
    }

    // Ensure ingestion workers see cancellation even if serve returned an
    // error without signal-driven shutdown. CancellationToken::cancel is
    // idempotent, so this is safe when the signal handler already fired.
    shutdown_token.cancel();
    // Drain the ingestion workers so buffered samples/snapshots have a
    // chance to land before the process exits.
    ingestion.join().await;
    info!("ingestion pipeline drained");

    info!("meshmon-service shutdown complete");
    Ok(())
}

/// Cheap `GET /` probe used for reachability warnings. Non-200 is still a
/// success for the reachability check — we only care that the DNS resolves
/// and the TCP/TLS handshake completes.
async fn probe(base: &str, name: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;
    let resp = client
        .get(base)
        .send()
        .await
        .with_context(|| format!("GET {base} ({name})"))?;
    info!(status = %resp.status(), url = %base, "{} reachable", name);
    Ok(())
}
