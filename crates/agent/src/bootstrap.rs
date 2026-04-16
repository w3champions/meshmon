//! Bootstrap sequence — registers the agent, fetches config and targets, and
//! spawns per-target supervisors. Handles retry with exponential backoff and
//! periodic reconciliation of the target list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use rand::Rng;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::api::ServiceApi;
use crate::config::{AgentEnv, AgentIdentity, ProbeConfig};
use crate::supervisor::{self, SupervisorHandle};
use meshmon_protocol::{RegisterRequest, Target};

// ---------------------------------------------------------------------------
// AgentRuntime
// ---------------------------------------------------------------------------

/// Owns the agent's supervisors, config broadcast channel, and cancellation
/// token. Created by [`AgentRuntime::bootstrap`] after successful registration
/// and initial target fetch.
///
/// Generic over `A: ServiceApi` because the trait uses `async fn` and is not
/// dyn-compatible. Production code passes `GrpcServiceApi`; tests pass mocks.
pub struct AgentRuntime<A: ServiceApi> {
    env: AgentEnv,
    api: Arc<A>,
    config_tx: watch::Sender<ProbeConfig>,
    config_rx: watch::Receiver<ProbeConfig>,
    supervisors: HashMap<String, SupervisorHandle>,
    cancel: CancellationToken,
}

impl<A: ServiceApi> AgentRuntime<A> {
    /// Register with the service, fetch config and targets, and spawn one
    /// supervisor per target. Each step retries with exponential backoff until
    /// success or cancellation.
    pub async fn bootstrap(env: AgentEnv, api: Arc<A>, cancel: CancellationToken) -> Result<Self> {
        // -- Register --
        let reg_req = build_register_request(&env.identity, &env.agent_version);
        retry_with_backoff(
            || {
                let req = reg_req.clone();
                let api = Arc::clone(&api);
                async move {
                    api.register(req).await?;
                    Ok(())
                }
            },
            &cancel,
            "register",
        )
        .await?;

        // -- Fetch config --
        let config_resp = retry_with_backoff(
            || {
                let api = Arc::clone(&api);
                async move { api.get_config().await }
            },
            &cancel,
            "get_config",
        )
        .await?;

        let probe_config = ProbeConfig::from_proto(config_resp);
        let (config_tx, config_rx) = watch::channel(probe_config);

        // -- Fetch targets --
        let source_id = env.identity.id.clone();
        let targets_resp = retry_with_backoff(
            || {
                let api = Arc::clone(&api);
                let sid = source_id.clone();
                async move { api.get_targets(&sid).await }
            },
            &cancel,
            "get_targets",
        )
        .await?;

        // -- Spawn supervisors (skip self and duplicates) --
        let mut supervisors = HashMap::new();
        for target in targets_resp.targets {
            if target.id == env.identity.id || supervisors.contains_key(&target.id) {
                continue;
            }
            let id = target.id.clone();
            let handle = supervisor::spawn(target, config_rx.clone(), cancel.clone());
            supervisors.insert(id, handle);
        }

        tracing::info!(
            supervisor_count = supervisors.len(),
            "bootstrap complete — supervisors spawned",
        );

        Ok(Self {
            env,
            api,
            config_tx,
            config_rx,
            supervisors,
            cancel,
        })
    }

    /// Run the periodic config/target refresh loop. Ticks every 5 minutes.
    /// Returns when the cancellation token fires.
    pub async fn run_refresh_loop(&mut self) {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // Skip the first immediate tick — we just bootstrapped.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::info!("refresh loop cancelled");
                    break;
                }
                _ = interval.tick() => {
                    self.refresh_once().await;
                }
            }
        }
    }

    /// Re-fetch config and targets from the service. Failures are logged as
    /// warnings; the agent continues with its previous state.
    ///
    /// Each RPC is raced against cancellation so a hung call cannot block the
    /// refresh loop (and therefore shutdown) during network stalls.
    async fn refresh_once(&mut self) {
        // -- Refresh config --
        let config_result = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                tracing::debug!("refresh: config fetch cancelled");
                return;
            }
            result = self.api.get_config() => result,
        };
        match config_result {
            Ok(resp) => {
                let new_config = ProbeConfig::from_proto(resp);
                // send() only fails if all receivers are dropped, which
                // cannot happen while we still hold config_rx.
                self.config_tx.send(new_config).ok();
                tracing::debug!("config refreshed");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to refresh config — keeping old");
            }
        }

        // -- Refresh targets --
        let targets_result = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                tracing::debug!("refresh: target fetch cancelled");
                return;
            }
            result = self.api.get_targets(&self.env.identity.id) => result,
        };
        match targets_result {
            Ok(resp) => {
                self.reconcile_targets(resp.targets).await;
                tracing::debug!(
                    supervisor_count = self.supervisors.len(),
                    "targets reconciled",
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to refresh targets — keeping old");
            }
        }
    }

    /// Reconcile the running supervisors with the new target list: remove stale
    /// supervisors and spawn new ones.
    async fn reconcile_targets(&mut self, targets: Vec<Target>) {
        let desired: HashSet<String> = targets
            .iter()
            .filter(|t| t.id != self.env.identity.id)
            .map(|t| t.id.clone())
            .collect();

        // -- Remove supervisors for targets no longer in the list --
        let stale_ids: Vec<String> = self
            .supervisors
            .keys()
            .filter(|id| !desired.contains(id.as_str()))
            .cloned()
            .collect();

        for id in stale_ids {
            if let Some(handle) = self.supervisors.remove(&id) {
                handle.cancel.cancel();
                // Keep an abort handle so we can force-kill the task if it
                // doesn't honor cancellation within the timeout. Without this,
                // dropping the JoinHandle on timeout would leak the task —
                // it would keep running (still probing a removed target) with
                // no way to stop it.
                let abort_handle = handle.join.abort_handle();
                match tokio::time::timeout(Duration::from_secs(5), handle.join).await {
                    Ok(Ok(())) => {
                        tracing::info!(target_id = %id, "supervisor stopped cleanly");
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(target_id = %id, error = %e, "supervisor panicked");
                    }
                    Err(_) => {
                        tracing::warn!(
                            target_id = %id,
                            "supervisor did not stop within 5s — aborting",
                        );
                        abort_handle.abort();
                    }
                }
            }
        }

        // -- Spawn supervisors for new targets --
        for target in targets {
            if target.id == self.env.identity.id {
                continue;
            }
            if self.supervisors.contains_key(&target.id) {
                continue;
            }
            let id = target.id.clone();
            let handle = supervisor::spawn(target, self.config_rx.clone(), self.cancel.clone());
            tracing::info!(target_id = %id, "spawned new supervisor");
            self.supervisors.insert(id, handle);
        }
    }

    /// Graceful shutdown: cancel all supervisors and await their completion.
    pub async fn shutdown(self) {
        self.cancel.cancel();

        for (id, handle) in self.supervisors {
            // Force-abort on timeout so stuck tasks can't outlive the runtime
            // without being terminated.
            let abort_handle = handle.join.abort_handle();
            match tokio::time::timeout(Duration::from_secs(10), handle.join).await {
                Ok(Ok(())) => {
                    tracing::info!(target_id = %id, "supervisor shut down cleanly");
                }
                Ok(Err(e)) => {
                    tracing::warn!(target_id = %id, error = %e, "supervisor panicked during shutdown");
                }
                Err(_) => {
                    tracing::warn!(
                        target_id = %id,
                        "supervisor did not stop within 10s during shutdown — aborting",
                    );
                    abort_handle.abort();
                }
            }
        }
    }

    /// Number of active supervisors. Useful for testing.
    pub fn supervisor_count(&self) -> usize {
        self.supervisors.len()
    }
}

// ---------------------------------------------------------------------------
// Helper: build RegisterRequest
// ---------------------------------------------------------------------------

fn build_register_request(id: &AgentIdentity, version: &str) -> RegisterRequest {
    RegisterRequest {
        id: id.id.clone(),
        display_name: id.display_name.clone(),
        location: id.location.clone(),
        ip: meshmon_protocol::ip::from_ipaddr(id.ip),
        lat: id.lat,
        lon: id.lon,
        agent_version: version.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Helper: retry with exponential backoff
// ---------------------------------------------------------------------------

/// Retry an async operation with exponential backoff (1s base, 30s max, ±25%
/// jitter). Returns the first successful result or an error if cancelled.
async fn retry_with_backoff<F, Fut, T>(
    mut op: F,
    cancel: &CancellationToken,
    label: &str,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut delay = Duration::from_secs(1);
    let max_delay = Duration::from_secs(30);

    loop {
        // Race the in-flight RPC against cancellation so a hung call (stalled
        // TCP/TLS handshake or unresponsive upstream) cannot block shutdown.
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                bail!("{label}: cancelled during RPC");
            }
            result = op() => result,
        };

        match outcome {
            Ok(val) => return Ok(val),
            Err(e) => {
                // Apply ±25% jitter.
                let jitter = rand::rng().random_range(0.75..1.25);
                let jittered = Duration::from_secs_f64(delay.as_secs_f64() * jitter);

                tracing::warn!(
                    label,
                    error = %e,
                    retry_in_ms = jittered.as_millis() as u64,
                    "retryable failure — backing off",
                );

                tokio::select! {
                    _ = cancel.cancelled() => {
                        bail!("{label}: cancelled during backoff");
                    }
                    _ = tokio::time::sleep(jittered) => {}
                }

                // Exponential growth, capped.
                delay = (delay * 2).min(max_delay);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use meshmon_protocol::{
        ConfigResponse, MetricsBatch, PushMetricsResponse, PushRouteSnapshotResponse,
        RegisterResponse, RouteSnapshotRequest, TargetsResponse,
    };

    // -- Helpers -------------------------------------------------------------

    fn test_env() -> AgentEnv {
        AgentEnv {
            service_url: "https://test.example.com".to_string(),
            agent_token: "test-token".to_string(),
            identity: AgentIdentity {
                id: "self-agent".to_string(),
                display_name: "Self Agent".to_string(),
                location: "Test Location".to_string(),
                ip: "10.0.0.100".parse::<IpAddr>().unwrap(),
                lat: 0.0,
                lon: 0.0,
            },
            agent_version: "0.1.0".to_string(),
        }
    }

    fn test_target(id: &str) -> Target {
        Target {
            id: id.to_string(),
            ip: vec![10, 0, 0, 1].into(),
            display_name: format!("Test {id}"),
            location: "Test".to_string(),
            lat: 0.0,
            lon: 0.0,
        }
    }

    // -- MockApi -------------------------------------------------------------

    struct MockApi {
        register_count: AtomicUsize,
        targets: Mutex<Vec<Target>>,
    }

    impl MockApi {
        fn new(targets: Vec<Target>) -> Arc<Self> {
            Arc::new(Self {
                register_count: AtomicUsize::new(0),
                targets: Mutex::new(targets),
            })
        }

        fn set_targets(&self, targets: Vec<Target>) {
            *self.targets.lock().unwrap() = targets;
        }
    }

    impl ServiceApi for MockApi {
        async fn register(&self, _req: RegisterRequest) -> Result<RegisterResponse> {
            self.register_count.fetch_add(1, Ordering::SeqCst);
            Ok(RegisterResponse {})
        }

        async fn get_config(&self) -> Result<ConfigResponse> {
            Ok(ConfigResponse::default())
        }

        async fn get_targets(&self, _source_id: &str) -> Result<TargetsResponse> {
            let targets = self.targets.lock().unwrap().clone();
            Ok(TargetsResponse { targets })
        }

        async fn push_metrics(&self, _batch: MetricsBatch) -> Result<PushMetricsResponse> {
            Ok(PushMetricsResponse {})
        }

        async fn push_route_snapshot(
            &self,
            _req: RouteSnapshotRequest,
        ) -> Result<PushRouteSnapshotResponse> {
            Ok(PushRouteSnapshotResponse {})
        }
    }

    // -- FailThenSucceedApi --------------------------------------------------

    struct FailThenSucceedApi {
        call_count: AtomicUsize,
        fail_count: usize,
    }

    impl FailThenSucceedApi {
        fn new(fail_count: usize) -> Arc<Self> {
            Arc::new(Self {
                call_count: AtomicUsize::new(0),
                fail_count,
            })
        }
    }

    impl ServiceApi for FailThenSucceedApi {
        async fn register(&self, _req: RegisterRequest) -> Result<RegisterResponse> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_count {
                bail!("transient failure #{}", n + 1);
            }
            Ok(RegisterResponse {})
        }

        async fn get_config(&self) -> Result<ConfigResponse> {
            Ok(ConfigResponse::default())
        }

        async fn get_targets(&self, _source_id: &str) -> Result<TargetsResponse> {
            Ok(TargetsResponse {
                targets: vec![test_target("peer-a")],
            })
        }

        async fn push_metrics(&self, _batch: MetricsBatch) -> Result<PushMetricsResponse> {
            Ok(PushMetricsResponse {})
        }

        async fn push_route_snapshot(
            &self,
            _req: RouteSnapshotRequest,
        ) -> Result<PushRouteSnapshotResponse> {
            Ok(PushRouteSnapshotResponse {})
        }
    }

    // -- Test 1 --------------------------------------------------------------

    #[tokio::test]
    async fn bootstrap_registers_and_spawns_supervisors() {
        let api = MockApi::new(vec![test_target("peer-a"), test_target("peer-b")]);
        let cancel = CancellationToken::new();

        let runtime = AgentRuntime::bootstrap(test_env(), api.clone(), cancel.clone())
            .await
            .expect("bootstrap should succeed");

        assert_eq!(api.register_count.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.supervisor_count(), 2);

        runtime.shutdown().await;
    }

    // -- Test 2 --------------------------------------------------------------

    #[tokio::test]
    async fn bootstrap_skips_self_target() {
        let api = MockApi::new(vec![test_target("self-agent"), test_target("peer-a")]);
        let cancel = CancellationToken::new();

        let runtime = AgentRuntime::bootstrap(test_env(), api, cancel.clone())
            .await
            .expect("bootstrap should succeed");

        assert_eq!(runtime.supervisor_count(), 1);

        runtime.shutdown().await;
    }

    // -- Test 3 --------------------------------------------------------------

    #[tokio::test]
    async fn reconcile_spawns_new_and_removes_old() {
        let api = MockApi::new(vec![test_target("peer-a")]);
        let cancel = CancellationToken::new();

        let mut runtime = AgentRuntime::bootstrap(test_env(), api.clone(), cancel.clone())
            .await
            .expect("bootstrap should succeed");

        assert_eq!(runtime.supervisor_count(), 1);

        // Change the target list.
        api.set_targets(vec![test_target("peer-b"), test_target("peer-c")]);
        runtime.refresh_once().await;

        assert_eq!(runtime.supervisor_count(), 2);
        assert!(
            !runtime.supervisors.contains_key("peer-a"),
            "peer-a should have been removed"
        );
        assert!(
            runtime.supervisors.contains_key("peer-b"),
            "peer-b should be present"
        );
        assert!(
            runtime.supervisors.contains_key("peer-c"),
            "peer-c should be present"
        );

        runtime.shutdown().await;
    }

    // -- Test 4 --------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn bootstrap_retries_on_transient_failure() {
        let api = FailThenSucceedApi::new(2);
        let cancel = CancellationToken::new();

        let runtime = AgentRuntime::bootstrap(test_env(), api.clone(), cancel.clone())
            .await
            .expect("bootstrap should succeed after retries");

        assert!(
            api.call_count.load(Ordering::SeqCst) >= 3,
            "should have called register at least 3 times"
        );
        assert_eq!(runtime.supervisor_count(), 1);

        runtime.shutdown().await;
    }
}
