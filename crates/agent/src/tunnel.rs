//! Long-lived reverse-tunnel task.
//!
//! Opens an `OpenTunnel` RPC, serves the `AgentCommand` service over the
//! yamux substreams tonic multiplexes inside it. On any termination —
//! clean stream end, yamux error, service disconnect — reconnects with
//! 1 s → 60 s exponential backoff plus ±25 % jitter. Any session that
//! stayed open for at least 10 s resets the delay back to the 1 s base.

use std::sync::Arc;
use std::time::{Duration, Instant};

use meshmon_protocol::AgentCommandServer;
use meshmon_revtunnel::TunnelClient;
use rand::Rng;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tracing::{debug, warn};

use crate::api::GrpcServiceApi;
use crate::command_service::RefreshConfigImpl;

const BASE_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(60);
const RESET_THRESHOLD: Duration = Duration::from_secs(10);

/// Spawn the tunnel task. Returns a join handle the caller can await
/// during shutdown.
pub fn spawn(
    api: Arc<GrpcServiceApi>,
    source_id: String,
    refresh_trigger: Arc<Notify>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(run(api, source_id, refresh_trigger, cancel))
}

async fn run(
    api: Arc<GrpcServiceApi>,
    source_id: String,
    refresh_trigger: Arc<Notify>,
    cancel: CancellationToken,
) {
    let mut delay = BASE_DELAY;
    loop {
        if cancel.is_cancelled() {
            return;
        }

        let session_start = Instant::now();
        let outcome = run_one_session(
            api.channel(),
            &source_id,
            &api.agent_token(),
            refresh_trigger.clone(),
            cancel.clone(),
        )
        .await;
        let session_duration = session_start.elapsed();
        match outcome {
            Ok(()) => debug!(
                duration_ms = session_duration.as_millis() as u64,
                "tunnel ended cleanly; reconnecting"
            ),
            Err(e) => warn!(
                duration_ms = session_duration.as_millis() as u64,
                error = %e,
                "tunnel errored; reconnecting"
            ),
        }

        if session_duration >= RESET_THRESHOLD {
            delay = BASE_DELAY;
        }

        let jitter = rand::rng().random_range(0.75..1.25);
        let jittered = Duration::from_secs_f64(delay.as_secs_f64() * jitter);

        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(jittered) => {}
        }

        delay = delay.saturating_mul(2).min(MAX_DELAY);
    }
}

async fn run_one_session(
    channel: tonic::transport::Channel,
    source_id: &str,
    agent_token: &str,
    refresh_trigger: Arc<Notify>,
    cancel: CancellationToken,
) -> Result<(), meshmon_revtunnel::TunnelError> {
    let router_factory = move || {
        Server::builder().add_service(AgentCommandServer::new(RefreshConfigImpl::new(
            refresh_trigger.clone(),
        )))
    };

    TunnelClient::open_and_run(channel, source_id, agent_token, router_factory, cancel).await
}

#[cfg(test)]
mod tests {
    //! Full coverage of the tunnel task requires a live tonic server +
    //! yamux session; those paths are exercised by the integration tests
    //! in `crates/service/tests/revtunnel_e2e.rs` (Task 15).
}
