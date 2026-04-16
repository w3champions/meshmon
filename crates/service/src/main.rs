//! `meshmon-service` binary entry point.
//!
//! Startup order (spec 03 §Startup):
//! 1. Load config (`$MESHMON_CONFIG`, default `/etc/meshmon/meshmon.toml`).
//! 2. Initialize tracing subscriber.
//! 3. Connect Postgres + run migrations.
//! 4. Best-effort reachability checks for VictoriaMetrics and Alertmanager
//!    (warn on failure — those upstreams may not exist yet during rollout).
//! 5. Agent registry — fail-fast initial load from DB.
//! 6. Bind HTTP listener.
//! 7. Build state + ingestion, mark ready, spawn registry refresh loop.
//! 8. Run the hyper-util auto::Builder server with graceful shutdown,
//!    driven by SIGTERM/SIGINT.  HTTP/1.1 (REST) and HTTP/2 (gRPC) are
//!    multiplexed on the same TCP port.

use anyhow::Context;
use arc_swap::ArcSwap;
use axum::extract::connect_info::ConnectInfo;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use meshmon_service::config::{Config, DEFAULT_CONFIG_PATH};
use meshmon_service::state::AppState;
use meshmon_service::{db, http, logging, shutdown};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
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

    // --- Step 5: Agent registry (fail-fast on first read) ---
    let registry_refresh_interval =
        std::time::Duration::from_secs(initial_config.agents.refresh_interval_seconds as u64);
    let registry_active_window = std::time::Duration::from_secs(
        (initial_config.agents.target_active_window_minutes as u64) * 60,
    );
    let registry = Arc::new(meshmon_service::registry::AgentRegistry::new(
        pool.clone(),
        registry_refresh_interval,
        registry_active_window,
    ));
    registry
        .initial_load()
        .await
        .context("initial agent registry load")?;
    info!(count = registry.snapshot().len(), "agent registry loaded");

    // --- Step 6: Bind HTTP listener ---
    let listen_addr: SocketAddr = initial_config.service.listen_addr;
    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("bind HTTP listener on {listen_addr}"))?;
    let local_addr = listener.local_addr()?;
    info!(addr = %local_addr, "HTTP listener bound");

    // Optionally load TLS config for standalone mode. The acceptor is
    // stored in an `ArcSwap` so SIGHUP reloads can swap in new certs
    // (e.g. after a Let's Encrypt renewal) without a process restart.
    // A failed reload keeps the previous acceptor and logs a warning
    // rather than forcing an outage.
    let initial_tls_acceptor = build_tls_acceptor(&initial_config).context("load TLS config")?;
    if initial_tls_acceptor.is_some() {
        info!(addr = %local_addr, "standalone TLS enabled");
    }
    let tls_swap: Arc<ArcSwap<Option<Arc<tokio_rustls::TlsAcceptor>>>> =
        Arc::new(ArcSwap::from(Arc::new(initial_tls_acceptor)));

    // --- Shutdown coordination ---
    // on_reload: re-read the config file and swap the ArcSwap. Detached by
    // shutdown.rs to keep the signal loop responsive, so rapid SIGHUPs could
    // otherwise race. A tokio Mutex serializes the spawned tasks — each
    // acquires the guard in FIFO order, so the most recent file read is the
    // last to land in ArcSwap. The TLS acceptor is rebuilt from the new
    // config's cert/key paths so certificate rotation on disk takes effect
    // without a process restart; a failed rebuild keeps the previous
    // acceptor so a broken reload can't take the service down.
    let reload_path = config_path.clone();
    let reload_handle = config_handle.clone();
    let reload_tx = config_tx.clone();
    let reload_lock = Arc::new(tokio::sync::Mutex::new(()));
    let reload_tls_swap = tls_swap.clone();
    let shutdown_token = shutdown::spawn(move || {
        let reload_path = reload_path.clone();
        let reload_handle = reload_handle.clone();
        let reload_tx = reload_tx.clone();
        let reload_lock = reload_lock.clone();
        let reload_tls_swap = reload_tls_swap.clone();
        async move {
            let _guard = reload_lock.lock().await;
            let new_cfg = match Config::from_file(&reload_path) {
                Ok(cfg) => Arc::new(cfg),
                Err(e) => {
                    warn!(error = %e, "config reload failed; keeping previous config");
                    return;
                }
            };
            // Validate and rebuild the TLS acceptor before touching any
            // observable state. Otherwise subscribers on `config_rx` would
            // see a `[agent_api.tls]` value that doesn't match the listener
            // (acceptor kept on the old cert while config advertises the
            // new one). Roll the entire reload back on TLS failure —
            // consistent with the "keep previous" policy for parse errors.
            let new_acceptor = match build_tls_acceptor(&new_cfg) {
                Ok(acceptor) => acceptor,
                Err(e) => {
                    warn!(
                        error = %e,
                        "tls rebuild failed during config reload; keeping previous config and acceptor",
                    );
                    return;
                }
            };
            reload_tls_swap.store(Arc::new(new_acceptor));
            reload_handle.store(new_cfg.clone());
            let _ = reload_tx.send(new_cfg);
            info!("config reloaded");
        }
    });

    // --- Step 7: Build state, mark ready, spawn refresh ---
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
    // Ingestion runs on a separate cancellation token from the HTTP
    // server. The HTTP server is drained first (so in-flight handlers can
    // finish pushing to the ingestion queues); only after serve returns is
    // `ingestion_token` cancelled. Sharing `shutdown_token` here would let
    // workers exit while handlers are still mid-push — silent data loss.
    let ingestion_token = CancellationToken::new();
    let ingestion = meshmon_service::ingestion::IngestionPipeline::spawn(
        ingestion_cfg,
        pool.clone(),
        ingestion_token.clone(),
    );

    // TODO(T10.11): move recorder install earlier in startup, alongside
    // `describe_service_metrics()` and `spawn_upkeep`. For now install it
    // here just so `AppState::new` has a handle to hold.
    let prom = meshmon_service::metrics::install_recorder();
    let state = AppState::new(
        config_handle,
        config_rx,
        pool,
        ingestion.clone(),
        registry.clone(),
        prom,
    );
    state.mark_ready();
    let registry_refresh = registry.clone().spawn_refresh(shutdown_token.clone());
    let app = http::router(state.clone());

    // --- Step 8: Serve with a bounded drain ---
    //
    // `hyper_util::server::conn::auto::Builder` detects the HTTP version on
    // each new TCP connection (HTTP/1.1 vs HTTP/2 / gRPC) and dispatches to
    // the appropriate hyper sub-server.  We accept connections in a loop,
    // injecting `ConnectInfo<SocketAddr>` into request extensions so that axum
    // extractors (`ConnectInfo`) and the tonic interceptor both see the peer IP.
    //
    // Graceful shutdown: when `shutdown_token` fires we stop accepting new
    // connections and enter an explicit drain phase. Per-connection tasks
    // are tracked in a `JoinSet`; we poll `join_next()` until the set is
    // empty or `shutdown_deadline` expires, at which point `conn_set.shutdown()`
    // aborts any stragglers. This gives unary RPCs time to finish writing
    // their response and is a prerequisite for long-lived streaming RPCs
    // (e.g. future T25 SubscribeCommands).
    let deadline = state.config().service.shutdown_deadline;

    let graceful_token = shutdown_token.clone();
    let graceful_state = state.clone();

    let serve_result: anyhow::Result<()> = {
        let builder = auto::Builder::new(TokioExecutor::new());
        // Clone once; each spawned task clones again from this handle.
        let app = app;
        let mut conn_set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        // Accept phase: run until the shutdown token fires. Also reap
        // finished connection tasks opportunistically so `conn_set` does
        // not grow unbounded on a long-lived server.
        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    let (tcp_stream, peer_addr) = match accept_result {
                        Ok(pair) => pair,
                        Err(e) => {
                            warn!(error = %e, "accept error; continuing");
                            continue;
                        }
                    };
                    // Read the current TLS acceptor once per connection; a
                    // concurrent SIGHUP reload will be visible on the next
                    // accept without interrupting this handshake.
                    let maybe_acceptor = tls_swap.load_full();
                    let conn_token = graceful_token.clone();
                    if let Some(acceptor) = maybe_acceptor.as_ref().clone() {
                        let app = app.clone();
                        let builder = builder.clone();
                        conn_set.spawn(async move {
                            match acceptor.accept(tcp_stream).await {
                                Ok(tls_stream) => {
                                    let io = TokioIo::new(tls_stream);
                                    serve_connection(io, peer_addr, app, builder, conn_token)
                                        .await;
                                }
                                Err(e) => {
                                    warn!(peer = %peer_addr, error = %e, "TLS handshake failed");
                                }
                            }
                        });
                    } else {
                        let io = TokioIo::new(tcp_stream);
                        let app = app.clone();
                        let builder = builder.clone();
                        conn_set.spawn(async move {
                            serve_connection(io, peer_addr, app, builder, conn_token).await;
                        });
                    }
                }
                Some(join_result) = conn_set.join_next() => {
                    if let Err(e) = join_result {
                        if e.is_panic() {
                            warn!(error = %e, "connection task panicked");
                        }
                    }
                }
                _ = graceful_token.cancelled() => {
                    graceful_state.mark_not_ready();
                    info!(pending = conn_set.len(), "HTTP server draining");
                    break;
                }
            }
        }

        // Drain phase: wait for in-flight connections to finish, bounded by
        // `shutdown_deadline`. On timeout we abort any stragglers so the
        // process can make progress.
        let drain_start = std::time::Instant::now();
        let drain_deadline = tokio::time::sleep(deadline);
        tokio::pin!(drain_deadline);
        loop {
            tokio::select! {
                join_result = conn_set.join_next() => match join_result {
                    Some(Err(e)) if e.is_panic() => {
                        warn!(error = %e, "connection task panicked during drain");
                    }
                    Some(_) => {}
                    None => {
                        info!(
                            elapsed_ms = drain_start.elapsed().as_millis() as u64,
                            "HTTP server drained cleanly",
                        );
                        break;
                    }
                },
                _ = &mut drain_deadline => {
                    warn!(
                        deadline_ms = deadline.as_millis() as u64,
                        pending = conn_set.len(),
                        "HTTP server did not drain within shutdown_deadline; aborting in-flight connections",
                    );
                    conn_set.shutdown().await;
                    break;
                }
            }
        }
        Ok(())
    };

    // If the accept loop returned an error before a signal fired, shutdown_token
    // was never cancelled. Signal it now so the registry refresh loop exits
    // promptly instead of waiting for the full `tokio::time::timeout` deadline.
    // No-op if already cancelled.
    shutdown_token.cancel();

    // HTTP is done — no more in-flight handlers can call `push_metrics` /
    // `push_snapshot`. Now cancel ingestion and drain any remaining
    // buffered samples/snapshots.
    ingestion_token.cancel();
    ingestion.join().await;
    info!("ingestion pipeline drained");

    // Registry refresh loop: cancelled by `shutdown_token`. The loop checks
    // the token after each `refresh_once` completes, so an in-flight refresh
    // runs to completion before the task exits. That extra query is bounded
    // by sqlx's connect timeout and has no drain semantics. We cap the wait
    // with the same shutdown_deadline used for HTTP so a stuck DB query can't
    // hang the process indefinitely.
    match tokio::time::timeout(deadline, registry_refresh).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "registry refresh task ended abnormally"),
        Err(_) => {
            warn!(
                deadline_ms = deadline.as_millis() as u64,
                "registry refresh did not drain within shutdown_deadline; aborting",
            );
        }
    }
    info!("agent registry refresh loop drained");

    serve_result.context("HTTP server")?;

    info!("meshmon-service shutdown complete");
    Ok(())
}

/// Drive a single accepted connection to completion.
///
/// Build a `TlsAcceptor` from the currently-resolved `[agent_api.tls]`
/// section, or `None` when TLS is not configured. Called at startup and
/// re-called from the SIGHUP reload closure so that cert/key files
/// replaced on disk are picked up without a process restart.
fn build_tls_acceptor(
    cfg: &Config,
) -> Result<Option<Arc<tokio_rustls::TlsAcceptor>>, meshmon_service::error::BootError> {
    let Some(tls_cfg) = cfg.agent_api.tls.as_ref() else {
        return Ok(None);
    };
    let server_cfg =
        meshmon_service::tls::load_rustls_config(&tls_cfg.cert_path, &tls_cfg.key_path)?;
    Ok(Some(Arc::new(tokio_rustls::TlsAcceptor::from(server_cfg))))
}

/// Injects `ConnectInfo<SocketAddr>` into every request's extensions so that
/// axum's `ConnectInfo` extractor and the tonic interceptor both see the peer
/// address without needing `into_make_service_with_connect_info`.
///
/// `I` must satisfy hyper's `Read + Write + Unpin + Send + 'static` bounds,
/// which both `TokioIo<TcpStream>` and `TokioIo<TlsStream<TcpStream>>` do.
///
/// `graceful_token` lets the drain phase nudge each connection into
/// `graceful_shutdown` — HTTP/2 connections get a GOAWAY frame so clients
/// stop opening new streams, and HTTP/1.1 keep-alive connections close
/// after the current response. Without this, `conn_set` would only bound
/// the accept queue: already-established connections could keep firing
/// fresh RPCs up until the deadline abort.
async fn serve_connection<I>(
    io: I,
    peer_addr: SocketAddr,
    app: axum::Router,
    builder: auto::Builder<TokioExecutor>,
    graceful_token: CancellationToken,
) where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    // Wrap the axum router in a closure that injects `ConnectInfo` before
    // dispatching to the router.  `axum::Router<()>` implements `Clone` and
    // `Service<Request<Body>>`, so cloning per-connection is cheap (Arc bump).
    let svc = tower::service_fn(move |mut req: hyper::Request<Incoming>| {
        let mut app = app.clone();
        async move {
            req.extensions_mut().insert(ConnectInfo(peer_addr));
            // axum::Router implements Service<Request<axum::body::Body>>, but
            // hyper gives us Request<Incoming>.  axum::body::Body wraps
            // Incoming, so we map the body type first.
            let req = req.map(axum::body::Body::new);
            use tower::Service as _;
            app.call(req).await
        }
    });

    let conn = builder.serve_connection_with_upgrades(io, TowerToHyperService::new(svc));
    tokio::pin!(conn);

    let result = tokio::select! {
        biased;
        res = conn.as_mut() => res,
        _ = graceful_token.cancelled() => {
            // Signal the remote peer and drain in-flight work. Per hyper-util
            // docs, the connection must continue to be polled until
            // graceful_shutdown can finish.
            conn.as_mut().graceful_shutdown();
            conn.as_mut().await
        }
    };

    if let Err(e) = result {
        // Connection errors (client disconnect, protocol mismatch) are
        // common in production; log at debug to avoid noise.
        tracing::debug!(peer = %peer_addr, error = %e, "connection error");
    }
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
