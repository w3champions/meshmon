//! Agent-side implementation of the `AgentCommand` tonic service.
//!
//! The service calls methods on this type through the reverse tunnel
//! (see `crate::tunnel`). Today the only method is `RefreshConfig`, which
//! wakes the refresh loop so the agent fetches fresh config + targets
//! without waiting for its 5-minute periodic poll.

use std::sync::Arc;

use meshmon_protocol::{AgentCommand, RefreshConfigRequest, RefreshConfigResponse};
use tokio::sync::Notify;
use tonic::{Request, Response, Status};
use tracing::info;

/// Service impl for `AgentCommand`. Owns a shared `Arc<Notify>` that the
/// refresh loop (wired in `bootstrap::run_refresh_loop`) awaits alongside
/// its interval timer.
pub struct RefreshConfigImpl {
    refresh_trigger: Arc<Notify>,
}

impl RefreshConfigImpl {
    /// Build a new impl bound to `refresh_trigger`. Clone the same `Arc`
    /// into the refresh loop so both sides observe the same `Notify`.
    pub fn new(refresh_trigger: Arc<Notify>) -> Self {
        Self { refresh_trigger }
    }
}

#[tonic::async_trait]
impl AgentCommand for RefreshConfigImpl {
    async fn refresh_config(
        &self,
        _request: Request<RefreshConfigRequest>,
    ) -> Result<Response<RefreshConfigResponse>, Status> {
        info!("received RefreshConfig; waking refresh loop");
        self.refresh_trigger.notify_one();
        Ok(Response::new(RefreshConfigResponse {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn refresh_config_wakes_the_trigger() {
        let trigger = Arc::new(Notify::new());
        let svc = RefreshConfigImpl::new(trigger.clone());

        // Arm the notified future before firing so we observe the wake
        // (Notify is level-triggered on pending waiters).
        let notified = trigger.notified();
        tokio::pin!(notified);

        svc.refresh_config(Request::new(RefreshConfigRequest {}))
            .await
            .expect("handler returned Ok");

        tokio::time::timeout(Duration::from_millis(100), notified.as_mut())
            .await
            .expect("notified future resolved within 100ms");
    }

    #[tokio::test]
    async fn refresh_config_returns_empty_response() {
        let trigger = Arc::new(Notify::new());
        let svc = RefreshConfigImpl::new(trigger);
        let response = svc
            .refresh_config(Request::new(RefreshConfigRequest {}))
            .await
            .expect("handler ok");
        // RefreshConfigResponse has no fields today, but verify the
        // response envelope reaches the caller cleanly.
        let _empty = response.into_inner();
    }
}
