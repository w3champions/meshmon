// crates/agent/src/main.rs
//! meshmon-agent binary entry point.

use anyhow::Result;
use meshmon_agent::api::GrpcServiceApi;
use meshmon_agent::bootstrap::AgentRuntime;
use meshmon_agent::config::AgentEnv;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
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

    // Bootstrap: register, fetch config + targets, spawn supervisors.
    let mut runtime = AgentRuntime::bootstrap(env, api, cancel.clone()).await?;

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
