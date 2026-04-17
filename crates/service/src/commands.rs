//! Service → agent command fan-out driven by SIGHUP config reloads.
//!
//! When `config_rx` fires, we snapshot the active tunnels and invoke
//! `AgentCommand::RefreshConfig` on each concurrently. Per-call deadline
//! 10 s; failures logged at WARN but not retried — the agent's 5-min
//! periodic poll is the safety net.

use std::sync::Arc;
use std::time::Duration;

use meshmon_protocol::{AgentCommandClient, RefreshConfigRequest};
use meshmon_revtunnel::TunnelManager;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::config::Config;

/// Per-RPC deadline for fan-out calls. Chosen so a stuck agent can't
/// wedge the watcher; 10 s is comfortably above p99 round-trip on healthy
/// tunnels and well below the 5-min config poll cadence that acts as the
/// safety net.
const REFRESH_CONFIG_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawn the watcher task. The caller keeps the returned `JoinHandle` so
/// it can await clean drain during service shutdown.
pub fn spawn_config_watcher(
    manager: Arc<TunnelManager>,
    mut config_rx: watch::Receiver<Arc<Config>>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Ignore the initial value — only notify on subsequent changes.
        config_rx.mark_unchanged();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("config watcher cancelled");
                    return;
                }
                change = config_rx.changed() => match change {
                    Ok(()) => broadcast_refresh(&manager).await,
                    Err(_) => {
                        debug!("config sender dropped; watcher exiting");
                        return;
                    }
                }
            }
        }
    })
}

async fn broadcast_refresh(manager: &Arc<TunnelManager>) {
    let agents = manager.snapshot();
    if agents.is_empty() {
        return;
    }
    debug!(count = agents.len(), "broadcasting RefreshConfig");

    let mut tasks = Vec::with_capacity(agents.len());
    for (id, channel) in agents {
        tasks.push(tokio::spawn(async move {
            let mut client = AgentCommandClient::new(channel);
            let mut req = tonic::Request::new(RefreshConfigRequest {});
            req.set_timeout(REFRESH_CONFIG_TIMEOUT);
            match client.refresh_config(req).await {
                Ok(_) => {
                    crate::metrics::command_rpcs(
                        "refresh_config",
                        crate::metrics::CommandOutcome::Ok,
                    )
                    .increment(1);
                }
                Err(status) => {
                    let outcome = match status.code() {
                        tonic::Code::Unavailable => crate::metrics::CommandOutcome::Unavailable,
                        tonic::Code::DeadlineExceeded => {
                            crate::metrics::CommandOutcome::DeadlineExceeded
                        }
                        _ => crate::metrics::CommandOutcome::Other,
                    };
                    crate::metrics::command_rpcs("refresh_config", outcome).increment(1);
                    warn!(
                        agent_id = %id,
                        code = ?status.code(),
                        error = %status,
                        "RefreshConfig failed",
                    );
                }
            }
        }));
    }
    for task in tasks {
        let _ = task.await;
    }
}
