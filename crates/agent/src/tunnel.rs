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
use crate::command::{AgentCommandService, StubProber};

const BASE_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(60);
const RESET_THRESHOLD: Duration = Duration::from_secs(10);

/// Fallback agent campaign concurrency cap when the operator sets no
/// `MESHMON_CAMPAIGN_MAX_CONCURRENCY` override. Matches the service-side
/// `[campaigns.default_agent_concurrency]` default so an agent that
/// doesn't override sees the same cap the dispatcher enforces.
const DEFAULT_AGENT_CAMPAIGN_CONCURRENCY: usize = 16;

/// Spawn the tunnel task. Returns a join handle the caller can await
/// during shutdown.
///
/// `campaign_max_concurrency` is the per-agent cap on concurrent
/// in-flight campaign measurement batches; `None` falls back to
/// [`DEFAULT_AGENT_CAMPAIGN_CONCURRENCY`]. The value feeds the tonic
/// service's semaphore — probes above the cap get
/// `Status::resource_exhausted`, which the dispatcher treats as a
/// rejection so the scheduler reverts the pairs.
pub fn spawn(
    api: Arc<GrpcServiceApi>,
    source_id: String,
    refresh_trigger: Arc<Notify>,
    campaign_max_concurrency: Option<u32>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    let effective = campaign_max_concurrency
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_AGENT_CAMPAIGN_CONCURRENCY);
    tokio::spawn(run(api, source_id, refresh_trigger, effective, cancel))
}

async fn run(
    api: Arc<GrpcServiceApi>,
    source_id: String,
    refresh_trigger: Arc<Notify>,
    max_concurrency: usize,
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
            max_concurrency,
            cancel.clone(),
        )
        .await;
        let session_duration = session_start.elapsed();
        match outcome {
            Ok(()) => debug!(
                duration_ms = session_duration.as_millis() as u64,
                "tunnel ended cleanly; reconnecting"
            ),
            // Auth failures stand out: they indicate a misconfigured
            // source_id or bearer token, which would otherwise be
            // indistinguishable from a transient network blip in ops logs.
            // We keep retrying (operators can hot-fix the token) but make
            // the cause obvious.
            Err(meshmon_revtunnel::TunnelError::AuthFailed(status)) => warn!(
                duration_ms = session_duration.as_millis() as u64,
                status = %status,
                "tunnel auth rejected; check source_id / agent token — reconnecting"
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
    max_concurrency: usize,
    cancel: CancellationToken,
) -> Result<(), meshmon_revtunnel::TunnelError> {
    // `StubProber` is the default; it's the T45 transport-test seam and
    // will be swapped for a real trippy-backed prober at this same call
    // site in T46 without changing any other wiring.
    let prober = Arc::new(StubProber);
    let router_factory = move || {
        Server::builder().add_service(AgentCommandServer::new(AgentCommandService::new(
            refresh_trigger.clone(),
            prober.clone(),
            max_concurrency,
        )))
    };

    TunnelClient::open_and_run(channel, source_id, agent_token, router_factory, cancel).await
}

#[cfg(test)]
mod tests {
    //! Full coverage of the tunnel task requires a live tonic server +
    //! yamux session; those paths are exercised by the integration tests
    //! in `crates/service/tests/revtunnel_e2e.rs`.
}
