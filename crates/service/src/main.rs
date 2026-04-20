//! `meshmon-service` binary entry point.
//!
//! Startup order (spec 03 §Startup):
//! 1. Load config (`$MESHMON_CONFIG`, default `/etc/meshmon/meshmon.toml`).
//! 2. Initialize tracing subscriber.
//!    2b. Install Prometheus recorder, describe self-metrics, and emit
//!    `meshmon_service_build_info` so every subsystem's `metrics::*`
//!    calls feed the installed handle from the first moment they can fire.
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
use meshmon_service::enrichment::providers::build_chain;
use meshmon_service::enrichment::runner::{EnrichmentQueue, Runner};
use meshmon_service::state::AppState;
use meshmon_service::{db, http, logging, shutdown};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Periodic sweep interval for the enrichment runner. Matches the 30 s
/// staleness window enforced by `Runner::sweep`.
const ENRICHMENT_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Bounded capacity for the enrichment work queue. Sized so a short
/// paste burst can enqueue without back-pressure while bounding memory.
const ENRICHMENT_QUEUE_CAPACITY: usize = 1024;

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

    // --- Step 2b: Metrics recorder ---
    // Install BEFORE ingestion/registry subsystems emit anything —
    // once installed, every `metrics::*` call in those modules feeds
    // this handle. `install_recorder` sets the process-global recorder
    // and panics on a second call; this binary calls it exactly once
    // here. Integration tests go through `crate::metrics::test_install`
    // which shares the handle via `OnceLock`.
    let prom = meshmon_service::metrics::install_recorder();
    meshmon_service::metrics::describe_service_metrics();
    meshmon_service::metrics::emit_build_info(meshmon_service::state::BuildInfo::compile_time());
    // Seed the probe-collision counter at zero so scrapers see the
    // series from boot even before the agent's real prober lands in
    // T46. `absolute(0)` is a no-op on the counter's value but
    // registers the zero-labeled series with the exporter.
    meshmon_service::metrics::campaign_probe_collisions_total().absolute(0);

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
    // Snapshot startup-captured URLs for change detection on reload.
    // See the module-level comment in `http/grafana_proxy.rs` —
    // `axum-reverse-proxy` binds the target URL at construction and a
    // rebuild-on-reload would mean swapping the live `Router`, which
    // isn't supported by the current server plumbing. Instead, warn
    // loudly when the URL changes so operators know a restart is
    // required for the new value to take effect.
    let boot_grafana_url = initial_config.upstream.grafana_url.clone();
    let boot_alertmanager_url = initial_config.upstream.alertmanager_url.clone();
    let shutdown_token = shutdown::spawn(move || {
        let reload_path = reload_path.clone();
        let reload_handle = reload_handle.clone();
        let reload_tx = reload_tx.clone();
        let reload_lock = reload_lock.clone();
        let reload_tls_swap = reload_tls_swap.clone();
        let boot_grafana_url = boot_grafana_url.clone();
        let boot_alertmanager_url = boot_alertmanager_url.clone();
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
            if new_cfg.upstream.grafana_url != boot_grafana_url {
                warn!(
                    "upstream.grafana_url changed on SIGHUP reload; \
                     /grafana/* continues routing to the startup target \
                     until the service restarts"
                );
            }
            if new_cfg.upstream.alertmanager_url != boot_alertmanager_url {
                warn!(
                    "upstream.alertmanager_url changed on SIGHUP reload; \
                     /alertmanager/* continues routing to the startup target \
                     until the service restarts"
                );
            }
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

    // Enrichment chain: construct the provider chain from config. A
    // misconfigured section (e.g. `ipgeolocation.enabled = true` without
    // a resolved API key) is logged rather than aborting boot — the
    // service still serves the rest of the API, and the runner keeps
    // running against an empty chain so the sweep still moves
    // `pending` rows to `enriched` via whichever providers did resolve.
    // Fail startup when `build_chain` can't construct the configured
    // chain. An earlier revision swallowed the error and continued with
    // an empty chain — but a single misconfigured provider
    // (e.g. ipgeolocation enabled without `api_key_env`) would then
    // silently disable *every* provider, including the correctly-configured
    // ones, and the operator would see warn logs while production
    // enrichment globally stopped. That failure mode is strictly worse
    // than the no-enrichment mode, so we surface it at boot the same
    // way missing `acknowledged_tos` does in the config loader. The
    // operator can disable the broken provider explicitly to return to
    // a healthy chain.
    let enrichment_chain =
        build_chain(&initial_config.enrichment).context("build enrichment provider chain")?;
    info!(
        providers = enrichment_chain.len(),
        "enrichment chain initialised",
    );
    let (enrichment_queue_raw, enrichment_rx) = EnrichmentQueue::new(ENRICHMENT_QUEUE_CAPACITY);
    let enrichment_queue = Arc::new(enrichment_queue_raw);

    // Build AppState before the scheduler so the dispatcher can borrow
    // the tunnel manager from it. `AppState::new` is pure construction
    // — no tasks are spawned — so the scheduler still comes up after
    // the state is fully assembled below.
    let state = AppState::new(
        config_handle,
        config_rx,
        pool.clone(),
        ingestion.clone(),
        registry.clone(),
        prom.clone(),
        enrichment_queue,
    );

    // Campaign scheduler: single tokio task, driven by a dedicated cancel
    // token so it can be shut down AFTER the HTTP drain completes (in-flight
    // handlers may still publish NOTIFY wake-ups until they finish). Gated
    // on `[campaigns] enabled` — operators flip this off in TOML to
    // suppress dispatch. The `RpcDispatcher` routes batches over the
    // per-agent yamux tunnels owned by `state.tunnel_manager`.
    let (campaign_cancel, campaign_scheduler_handle) = if initial_config.campaigns.enabled {
        let cancel = CancellationToken::new();
        let writer = meshmon_service::campaign::writer::SettleWriter::new(pool.clone());
        let dispatcher: Arc<dyn meshmon_service::campaign::dispatch::PairDispatcher> = Arc::new(
            meshmon_service::campaign::rpc_dispatcher::RpcDispatcher::new(
                state.tunnel_manager.clone(),
                registry.clone(),
                writer,
                initial_config.campaigns.default_agent_concurrency,
                initial_config.campaigns.per_destination_rps,
                initial_config.campaigns.max_batch_size,
            ),
        );
        let scheduler = meshmon_service::campaign::scheduler::Scheduler::new(
            pool.clone(),
            registry.clone(),
            dispatcher,
            initial_config.campaigns.scheduler_tick_ms,
            /* chunk_size = */ 64,
            initial_config.campaigns.per_destination_rps,
            // `max_pair_attempts` is `u16` in config but `i16` at the DB edge;
            // both map onto the same non-negative range in practice. Clamp
            // explicitly so a misconfigured `65535` can't wrap to a negative
            // attempts threshold.
            std::cmp::min(initial_config.campaigns.max_pair_attempts, i16::MAX as u16) as i16,
            registry_active_window,
        );
        let handle = tokio::spawn(scheduler.run(cancel.clone()));
        info!("campaign scheduler spawned (RpcDispatcher)");
        (Some(cancel), Some(handle))
    } else {
        info!("campaign scheduler disabled (set [campaigns] enabled = true to start)");
        (None, None)
    };

    let state = {
        let mut s = state;
        s.campaign_cancel = campaign_cancel.clone();
        s
    };
    state.mark_ready();

    // Spawn the enrichment runner. The runner terminates when the last
    // sender clone is dropped — `AppState` holds one via
    // `enrichment_queue`, so shutdown naturally drains: after the HTTP
    // server finishes, `state` drops, the sender is released, and the
    // runner's `rx.recv()` returns `None`. No shutdown token plumbing
    // needed; tracking the `JoinHandle` so the main task can await it
    // during the drain phase below.
    let enrichment_runner_handle = tokio::spawn(
        Runner::new(
            pool.clone(),
            enrichment_chain,
            state.catalogue_broker.clone(),
            enrichment_rx,
            ENRICHMENT_SWEEP_INTERVAL,
            Arc::clone(&state.facets_cache),
        )
        .run(),
    );

    let registry_refresh = registry.clone().spawn_refresh(shutdown_token.clone());
    let upkeep_handle =
        meshmon_service::metrics::spawn_upkeep(prom.clone(), shutdown_token.clone());
    let command_watcher_handle = meshmon_service::commands::spawn_config_watcher(
        state.tunnel_manager.clone(),
        state.config_rx.clone(),
        shutdown_token.clone(),
    );
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
                    // Signal tunnel drivers to exit BEFORE the HTTP drain
                    // phase. `OpenTunnel` response streams are long-lived —
                    // their per-connection tasks in `conn_set` won't EOF
                    // until the driver drops its outbound sender. Closing
                    // tunnels here lets those streams end cleanly and the
                    // drain loop below observes the connection tasks
                    // finishing within `shutdown_deadline`. The post-serve
                    // `close_all()` call further down is idempotent.
                    graceful_state.tunnel_manager.close_all();
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

    // Campaign scheduler: cancel after HTTP drain so in-flight handlers
    // can still publish NOTIFY wake-ups while serving their final
    // responses. The scheduler's `run` loop observes the cancel token on
    // every select arm and exits promptly; the `timeout` is a backstop
    // for a stuck Postgres query, not the expected path. Both `cancel`
    // and `handle` are `None` when the scheduler was never spawned
    // (`[campaigns] enabled = false`).
    if let (Some(cancel), Some(handle)) = (campaign_cancel, campaign_scheduler_handle) {
        cancel.cancel();
        match tokio::time::timeout(deadline, handle).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!(error = %e, "campaign scheduler task ended abnormally"),
            Err(_) => warn!(
                deadline_ms = deadline.as_millis() as u64,
                "campaign scheduler did not drain within shutdown_deadline; aborting",
            ),
        }
        info!("campaign scheduler drained");
    }

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

    match tokio::time::timeout(deadline, upkeep_handle).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "metrics upkeep task ended abnormally"),
        Err(_) => warn!(
            deadline_ms = deadline.as_millis() as u64,
            "metrics upkeep did not drain within shutdown_deadline; aborting",
        ),
    }
    info!("metrics upkeep loop drained");

    state.tunnel_manager.close_all();
    match tokio::time::timeout(deadline, command_watcher_handle).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "command watcher task ended abnormally"),
        Err(_) => warn!(
            deadline_ms = deadline.as_millis() as u64,
            "command watcher did not drain within shutdown_deadline; aborting",
        ),
    }
    info!("command watcher drained");

    // Drop the last stack-held `AppState` clones so the `Arc<EnrichmentQueue>`
    // sender count reaches zero. The runner's `rx.recv()` then returns `None`
    // and the task exits cleanly. Any handler clones of state were released
    // when the HTTP drain loop above finished, so this drop is the final one.
    drop(graceful_state);
    drop(state);
    match tokio::time::timeout(deadline, enrichment_runner_handle).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "enrichment runner task ended abnormally"),
        Err(_) => warn!(
            deadline_ms = deadline.as_millis() as u64,
            "enrichment runner did not drain within shutdown_deadline; aborting",
        ),
    }
    info!("enrichment runner drained");

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
