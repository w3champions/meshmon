// crates/agent/src/main.rs
//! meshmon-agent binary entry point.

use anyhow::Result;
use meshmon_agent::api::GrpcServiceApi;
use meshmon_agent::bootstrap::AgentRuntime;
use meshmon_agent::config::AgentEnv;
use tokio_util::sync::CancellationToken;

fn main() -> Result<()> {
    // Build the runtime with an explicit cap on the blocking pool.
    // Rationale: trippy rounds run under `spawn_blocking` and each holds
    // a thread for up to `read_timeout + grace`. Bounding the pool
    // prevents runaway thread + memory growth if trippy stalls under
    // network degradation. Stack size halved from tokio's 8 MB default
    // to bound worst-case RAM at ~128 MB rather than ~512 MB.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(64)
        .thread_stack_size(2 * 1024 * 1024)
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Tracing subscriber: structured JSON on stdout, configurable via
    // RUST_LOG (default: info for meshmon crates, warn for everything else).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "meshmon_agent=info,warn".parse().expect("valid filter")),
        )
        .json()
        .with_target(true)
        .init();

    // Parse env vars — fail fast if any are missing.
    let env = AgentEnv::from_env()?;
    tracing::info!(
        agent_id = %env.identity.id,
        service_url = %env.service_url,
        version = %env.agent_version,
        "starting meshmon-agent"
    );

    // Construct gRPC client.
    let api = GrpcServiceApi::connect(&env.service_url, &env.agent_token).await?;

    // Top-level cancellation token.
    let cancel = CancellationToken::new();

    // Wire up OS signal handlers.
    let cancel_for_signal = cancel.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("received shutdown signal");
        cancel_for_signal.cancel();
    });

    // Snapshot the agent ID before `env` is moved into bootstrap.
    let agent_id = env.identity.id.clone();

    // Bootstrap: register, fetch config + targets, spawn supervisors.
    // Clone `api` so both bootstrap and the tunnel task share the same
    // Arc<GrpcServiceApi> (clone is a cheap Arc bump).
    let mut runtime = AgentRuntime::bootstrap(env, api.clone(), cancel.clone()).await?;

    // Spawn the reverse-tunnel task and attach it to the runtime. The
    // tunnel runs against the concrete GrpcServiceApi; test mocks don't
    // expose the raw gRPC channel, so this wiring lives here rather than
    // inside bootstrap.
    let refresh_trigger = std::sync::Arc::new(tokio::sync::Notify::new());
    let tunnel_handle =
        meshmon_agent::tunnel::spawn(api, agent_id, refresh_trigger.clone(), cancel.clone());
    runtime.attach_tunnel(refresh_trigger, tunnel_handle);

    // Run the refresh loop (blocks until cancellation).
    runtime.run_refresh_loop().await;

    // Graceful shutdown.
    runtime.shutdown().await;
    tracing::info!("meshmon-agent stopped");

    Ok(())
}

/// Wait for SIGTERM or SIGINT (ctrl-c).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to register ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
