//! Bootstrap sequence — registers the agent, fetches config and targets, and
//! spawns per-target supervisors. Handles retry with exponential backoff and
//! periodic reconciliation of the target list.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rand::Rng;
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::api::ServiceApi;
use crate::config::{AgentEnv, AgentIdentity, ProbeConfig};
use crate::probing::echo_udp::SecretSnapshot;
use crate::probing::trippy::TrippyProber;
use crate::probing::udp::UdpProberPool;
use crate::probing::{echo_tcp, echo_udp};
use crate::route::RouteSnapshotEnvelope;
use crate::supervisor::{self, SupervisorHandle};
use meshmon_protocol::{ConfigResponse, RegisterRequest, Target, TargetsResponse};

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
    /// Broadcast of the UDP probe secret + optional previous. Populated
    /// by `GetConfig`, consumed by the UDP listener and prober pool.
    secret_tx: watch::Sender<SecretSnapshot>,
    /// Broadcast of the peer IP allowlist. Populated by `GetTargets`,
    /// consumed by the UDP listener.
    allowlist_tx: watch::Sender<Arc<HashSet<IpAddr>>>,
    /// UDP prober pool handle shared across per-target UDP probers.
    pub udp_pool: Arc<UdpProberPool>,
    /// Trippy shared prober (semaphore).
    pub trippy_prober: Arc<TrippyProber>,
    /// TCP echo listener handle. Held so the task is cancelled along with
    /// the cancellation token but not awaited synchronously.
    _tcp_listener: tokio::task::JoinHandle<()>,
    /// UDP echo listener handle. Held for the same reason as the TCP one.
    _udp_listener: tokio::task::JoinHandle<()>,
    /// Sender side of the shared route-snapshot channel. Handed to every
    /// per-target supervisor so each one can push envelopes (target_id +
    /// snapshot) into a single consumer. T16 will replace the placeholder
    /// consumer with the real emitter; until then this feeds
    /// [`run_route_snapshot_consumer`] which just logs each snapshot at
    /// `info`.
    route_snapshot_tx: mpsc::Sender<RouteSnapshotEnvelope>,
    /// Join handle for the placeholder snapshot consumer. Held for the
    /// runtime's lifetime so the task is cancelled when the runtime is
    /// dropped; not awaited in production (the `_` prefix signals that).
    _route_snapshot_consumer: tokio::task::JoinHandle<()>,
    /// Sender half of the supervisor → emitter metrics channel. Cloned into
    /// every supervisor at spawn time. Held on the runtime so its lifetime
    /// matches the supervisors; dropped explicitly in `shutdown()` (Task 11)
    /// so the emitter sees clean channel closure before draining.
    pub(crate) path_metrics_tx: mpsc::Sender<crate::emitter::PathMetricsMsg>,
    /// Receiver half of the metrics channel. Taken by Task 11 when the
    /// emitter is spawned; `Option<_>` because `emitter::spawn` consumes it.
    #[allow(dead_code)]
    pub(crate) path_metrics_rx: Option<mpsc::Receiver<crate::emitter::PathMetricsMsg>>,
}

impl<A: ServiceApi> AgentRuntime<A> {
    /// Register with the service, fetch config and targets, and spawn one
    /// supervisor per target. Each step retries with exponential backoff until
    /// success or cancellation.
    pub async fn bootstrap(env: AgentEnv, api: Arc<A>, cancel: CancellationToken) -> Result<Self> {
        // Derive a child cancel token. Every task spawned here (listeners,
        // UDP pool receiver/sweeper, trippy driver semaphore holders,
        // per-target supervisors) uses `child.clone()` so either (a) the
        // parent `cancel` firing or (b) us hitting the drop-guard on an
        // error-return propagates a stop signal. The parent's cancel flows
        // through `child` automatically via the child-token relationship;
        // storing `child` as `self.cancel` ensures `runtime.shutdown()`
        // only cancels our own sub-tree, not a token the caller still uses
        // for other work.
        let child = cancel.child_token();
        // Drop-guard: any `?`-return below cancels `child`, which tears
        // down every task we've spawned so far. Disarmed immediately
        // before the successful `Ok(Self{..})`. Without this, a late
        // bootstrap error (e.g. invalid `ConfigResponse` secret length)
        // would leave the listener tasks running and the ports bound,
        // making retries fail with `AddrInUse`.
        let cancel_guard = child.clone().drop_guard();

        // -- Spawn echo listeners FIRST so there is no window where peer
        // probes silently time out. Bind is eager: if a port is already in
        // use, bootstrap fails rather than registering against a dead
        // endpoint. The listeners start with an empty secret/allowlist
        // snapshot (they drop traffic until the GetConfig / GetTargets
        // steps below populate the watch channels).
        let (secret_tx, secret_rx) = watch::channel(SecretSnapshot::default());
        let (allowlist_tx, allowlist_rx) = watch::channel(Arc::new(HashSet::<IpAddr>::new()));
        let tcp_listener = echo_tcp::spawn(env.tcp_probe_port, child.clone())
            .await
            .with_context(|| {
                format!(
                    "tcp echo listener failed to bind on port {}",
                    env.tcp_probe_port
                )
            })?;
        let udp_listener = echo_udp::spawn(
            env.udp_probe_port,
            secret_rx.clone(),
            allowlist_rx,
            child.clone(),
        )
        .await
        .with_context(|| {
            format!(
                "udp echo listener failed to bind on port {}",
                env.udp_probe_port
            )
        })?;

        // UDP prober pool + trippy prober. Handles are owned by the runtime;
        // supervisors will attach targets via `spawn_target` in a later task.
        let udp_pool = UdpProberPool::new(secret_rx.clone(), child.clone())
            .await
            .context("UDP prober pool bind failed")?;
        let trippy_prober = TrippyProber::new(env.icmp_target_concurrency, child.clone());

        // Shared route-snapshot channel. Every supervisor clones the sender;
        // a single consumer task drains the receiver. Capacity 8 is
        // deliberately small: snapshots fire at most every 60 s per target,
        // so contention is rare — a larger buffer would just trade memory
        // for the time the consumer is behind without any real benefit.
        // The sender is stashed on the returned `AgentRuntime` so the
        // channel's lifetime matches the runtime (not this function).
        let (route_snapshot_tx, route_snapshot_rx) = mpsc::channel::<RouteSnapshotEnvelope>(8);
        let consumer_cancel = child.clone();
        let route_snapshot_consumer = tokio::spawn(run_route_snapshot_consumer(
            route_snapshot_rx,
            consumer_cancel,
        ));

        // Shared supervisor → emitter metrics channel. Capacity 1024 bounds how
        // many PathMetricsMsg records can queue up before the emitter catches
        // up — at 3 protocols × 50 targets / 60 s, steady-state use is 150/min
        // so 1024 gives >6 minutes of headroom. Overflow is `try_send` drop
        // at the supervisor side (per-target counter, local tracing only).
        let (path_metrics_tx, path_metrics_rx) =
            mpsc::channel::<crate::emitter::PathMetricsMsg>(1024);

        // -- Register --
        let reg_req = build_register_request(
            &env.identity,
            &env.agent_version,
            env.tcp_probe_port,
            env.udp_probe_port,
        );
        retry_with_backoff(
            || {
                let req = reg_req.clone();
                let api = Arc::clone(&api);
                async move {
                    api.register(req).await?;
                    Ok(())
                }
            },
            &child,
            "register",
            Duration::from_secs(1),
            Duration::from_secs(30),
        )
        .await?;

        // -- Fetch config --
        let config_resp = retry_with_backoff(
            || {
                let api = Arc::clone(&api);
                async move { api.get_config().await }
            },
            &child,
            "get_config",
            Duration::from_secs(1),
            Duration::from_secs(30),
        )
        .await?;

        let probe_config = ProbeConfig::from_proto(config_resp)
            .context("invalid ConfigResponse from service (bootstrap)")?;

        // -- Fetch targets --
        //
        // Important: we fetch targets BEFORE publishing the secret. Between
        // "secret set" and "allowlist set" the listener would authenticate
        // peer probes but see an empty allowlist — it'd then send rejection
        // packets, which put each peer into 60s `REJECTION_PAUSE`. Publishing
        // the allowlist first keeps the listener in silent-drop mode during
        // the `get_targets` RPC; once the secret lands the allowlist is
        // already populated, so the first valid probe is echoed without a
        // spurious rejection.
        let source_id = env.identity.id.clone();
        let targets_resp = retry_with_backoff(
            || {
                let api = Arc::clone(&api);
                let sid = source_id.clone();
                async move { api.get_targets(&sid).await }
            },
            &child,
            "get_targets",
            Duration::from_secs(1),
            Duration::from_secs(30),
        )
        .await?;

        // Publish the peer IP allowlist first — listener still drops
        // everything because the secret hasn't been published yet.
        publish_allowlist(&allowlist_tx, &targets_resp.targets);

        // Now publish the UDP probe secret. Listener + prober pool see
        // both a populated allowlist and a valid secret on the same update
        // batch (no inter-step refusal window). `send()` only fails when
        // all receivers are dropped — can't happen here since we still
        // hold `secret_tx`.
        let _ = secret_tx.send(SecretSnapshot {
            current: Some(probe_config.udp_probe_secret),
            previous: probe_config.udp_probe_previous_secret,
        });

        let (config_tx, config_rx) = watch::channel(probe_config);

        // -- Spawn supervisors (skip self and duplicates) --
        let mut supervisors = HashMap::new();
        for target in targets_resp.targets {
            if target.id == env.identity.id || supervisors.contains_key(&target.id) {
                continue;
            }
            let id = target.id.clone();
            let handle = supervisor::spawn(
                target,
                config_rx.clone(),
                Arc::clone(&udp_pool),
                Arc::clone(&trippy_prober),
                child.clone(),
                route_snapshot_tx.clone(),
                path_metrics_tx.clone(),
            );
            supervisors.insert(id, handle);
        }

        tracing::info!(
            supervisor_count = supervisors.len(),
            "bootstrap complete — supervisors spawned",
        );

        // Success: disarm the drop-guard so our spawned tasks keep running.
        // `child` (stored below as `self.cancel`) remains linked to the
        // parent, so the caller's token still cascades into shutdown.
        let cancel = cancel_guard.disarm();

        Ok(Self {
            env,
            api,
            config_tx,
            config_rx,
            supervisors,
            cancel,
            secret_tx,
            allowlist_tx,
            udp_pool,
            trippy_prober,
            _tcp_listener: tcp_listener,
            _udp_listener: udp_listener,
            route_snapshot_tx,
            _route_snapshot_consumer: route_snapshot_consumer,
            path_metrics_tx,
            path_metrics_rx: Some(path_metrics_rx),
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
    /// Each RPC is raced against cancellation and a per-attempt timeout so a
    /// hung call cannot block the refresh loop (and therefore shutdown) during
    /// network stalls.
    ///
    /// Ordering invariant: targets (→ allowlist) are refreshed BEFORE config
    /// (→ secret), mirroring the bootstrap invariant. Swapping them
    /// reintroduces a window where a freshly-registered peer passes the
    /// secret check but is absent from the allowlist, triggering a 60s
    /// `REJECTION_PAUSE` on that peer every refresh cycle.
    async fn refresh_once(&mut self) {
        // -- Refresh targets (and allowlist) FIRST --
        let targets_result: Result<TargetsResponse> = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                tracing::debug!("refresh: target fetch cancelled");
                return;
            }
            _ = tokio::time::sleep(RPC_ATTEMPT_TIMEOUT) => {
                Err(anyhow::anyhow!(
                    "get_targets timed out after {}s",
                    RPC_ATTEMPT_TIMEOUT.as_secs(),
                ))
            }
            result = self.api.get_targets(&self.env.identity.id) => result,
        };
        match targets_result {
            Ok(resp) => {
                // Re-publish the allowlist so the UDP listener tracks the
                // latest peer set (newcomers get echoed, removed peers will
                // start receiving `Refused` until they re-register).
                publish_allowlist(&self.allowlist_tx, &resp.targets);
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

        // -- Refresh config (and secret) SECOND --
        let config_result: Result<ConfigResponse> = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                tracing::debug!("refresh: config fetch cancelled");
                return;
            }
            _ = tokio::time::sleep(RPC_ATTEMPT_TIMEOUT) => {
                Err(anyhow::anyhow!(
                    "get_config timed out after {}s",
                    RPC_ATTEMPT_TIMEOUT.as_secs(),
                ))
            }
            result = self.api.get_config() => result,
        };
        match config_result {
            Ok(resp) => match ProbeConfig::from_proto(resp) {
                Ok(new_config) => {
                    // Re-publish the UDP secret so rotation propagates to the
                    // listener and the prober pool without a restart. By the
                    // time we get here the allowlist is already up to date
                    // (see ordering invariant on the function doc).
                    let _ = self.secret_tx.send(SecretSnapshot {
                        current: Some(new_config.udp_probe_secret),
                        previous: new_config.udp_probe_previous_secret,
                    });
                    // send() only fails if all receivers are dropped, which
                    // cannot happen while we still hold config_rx.
                    self.config_tx.send(new_config).ok();
                    tracing::debug!("config refreshed");
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "refresh config failed validation — keeping old",
                    );
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "failed to refresh config — keeping old");
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
            let handle = supervisor::spawn(
                target,
                self.config_rx.clone(),
                Arc::clone(&self.udp_pool),
                Arc::clone(&self.trippy_prober),
                self.cancel.clone(),
                self.route_snapshot_tx.clone(),
                self.path_metrics_tx.clone(),
            );
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
// Helper: route snapshot consumer (T15 placeholder, T16 replaces)
// ---------------------------------------------------------------------------

/// Placeholder consumer for route snapshots. T16 replaces this with the
/// real emitter that pushes snapshots to the service via
/// `push_route_snapshot`. Behaves lossily on shutdown: cancellation returns
/// immediately after a best-effort drain so a hung consumer can't delay
/// `AgentRuntime::shutdown`.
async fn run_route_snapshot_consumer(
    mut rx: mpsc::Receiver<RouteSnapshotEnvelope>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            maybe = rx.recv() => {
                match maybe {
                    Some(env) => {
                        tracing::info!(
                            target_id = %env.target_id,
                            protocol = ?env.snapshot.protocol,
                            hops = env.snapshot.hops.len(),
                            observed_at_micros = env.snapshot.observed_at_micros_i64(),
                            "route snapshot received (placeholder consumer, T15)",
                        );
                    }
                    None => break,
                }
            }
        }
    }
    // Best-effort drain of any in-flight snapshots so late observations
    // still surface in logs during shutdown.
    while let Ok(env) = rx.try_recv() {
        tracing::trace!(
            target_id = %env.target_id,
            protocol = ?env.snapshot.protocol,
            hops = env.snapshot.hops.len(),
            "draining route snapshot at shutdown",
        );
    }
}

// ---------------------------------------------------------------------------
// Helper: build RegisterRequest
// ---------------------------------------------------------------------------

fn build_register_request(
    id: &AgentIdentity,
    version: &str,
    tcp_probe_port: u16,
    udp_probe_port: u16,
) -> RegisterRequest {
    RegisterRequest {
        id: id.id.clone(),
        display_name: id.display_name.clone(),
        location: id.location.clone(),
        ip: meshmon_protocol::ip::from_ipaddr(id.ip),
        lat: id.lat,
        lon: id.lon,
        agent_version: version.to_string(),
        tcp_probe_port: tcp_probe_port as u32,
        udp_probe_port: udp_probe_port as u32,
    }
}

// ---------------------------------------------------------------------------
// Helper: publish peer IP allowlist
// ---------------------------------------------------------------------------

/// Extract the peer IPs from `targets` and publish them into `allowlist_tx`.
/// Targets whose `ip` field cannot be decoded are logged and skipped — they
/// will simply be omitted from the allowlist (peers with bogus IPs cannot
/// talk to us anyway).
fn publish_allowlist(allowlist_tx: &watch::Sender<Arc<HashSet<IpAddr>>>, targets: &[Target]) {
    // Canonicalize entries (`::ffff:a.b.c.d` → `Ipv4Addr`) so `contains()`
    // on the receiver side — which also canonicalizes — matches regardless
    // of whether the wire form was 4-byte v4 or 16-byte v4-mapped-v6. The
    // UDP echo listener applies the same canonicalization to incoming peer
    // IPs before the lookup.
    let allowlist: HashSet<IpAddr> = targets
        .iter()
        .filter_map(|t| match meshmon_protocol::ip::to_ipaddr(&t.ip) {
            Ok(ip) => Some(ip.to_canonical()),
            Err(e) => {
                tracing::warn!(
                    target_id = %t.id,
                    error = %e,
                    "skipping target with invalid ip in allowlist",
                );
                None
            }
        })
        .collect();
    // send() only fails if all receivers are dropped; the listener keeps its
    // receiver alive while the runtime exists.
    let _ = allowlist_tx.send(Arc::new(allowlist));
}

// ---------------------------------------------------------------------------
// Helper: retry with exponential backoff
// ---------------------------------------------------------------------------

/// Per-attempt deadline for each RPC inside [`retry_with_backoff`]. A hung
/// RPC (TCP connected but server never responds at the gRPC layer) would
/// otherwise block bootstrap forever; this deadline converts the hang into
/// a retryable error so the backoff/retry logic can actually run.
const RPC_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);

/// Retry an async operation with exponential backoff, ±25% jitter. Each
/// attempt is bounded by [`RPC_ATTEMPT_TIMEOUT`]. Returns the first
/// successful result, or an error if cancelled.
async fn retry_with_backoff<F, Fut, T>(
    mut op: F,
    cancel: &CancellationToken,
    label: &str,
    base_delay: Duration,
    max_delay: Duration,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut delay = base_delay;

    loop {
        // Race the in-flight RPC against:
        // - cancellation: so a hung call cannot block shutdown
        // - attempt timeout: so a hung call becomes a retryable failure
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                bail!("{label}: cancelled during RPC");
            }
            _ = tokio::time::sleep(RPC_ATTEMPT_TIMEOUT) => {
                Err(anyhow::anyhow!(
                    "{label}: RPC attempt timed out after {}s",
                    RPC_ATTEMPT_TIMEOUT.as_secs(),
                ))
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
                delay = (delay.saturating_mul(2)).min(max_delay);
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use meshmon_protocol::{
        ConfigResponse, MetricsBatch, PushMetricsResponse, PushRouteSnapshotResponse,
        RegisterResponse, RouteSnapshotRequest, TargetsResponse,
    };
    use tokio::net::{TcpListener, UdpSocket};

    // -- Helpers -------------------------------------------------------------

    /// Allocate an ephemeral TCP port and an ephemeral UDP port on the
    /// loopback interface. Binds then drops — the kernel unlikely hands
    /// out the same port to a concurrent test within the short window
    /// between `drop` and the real listener binding.
    ///
    /// Using ephemeral ports is required because the listeners bind to
    /// fixed ports (as specified by `AgentEnv`) and multiple bootstrap
    /// tests run in parallel inside the same test binary.
    async fn ephemeral_probe_ports() -> (u16, u16) {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tcp_port = tcp.local_addr().unwrap().port();
        drop(tcp);
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp_port = udp.local_addr().unwrap().port();
        drop(udp);
        (tcp_port, udp_port)
    }

    async fn test_env() -> AgentEnv {
        let (tcp_probe_port, udp_probe_port) = ephemeral_probe_ports().await;
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
            tcp_probe_port,
            udp_probe_port,
            icmp_target_concurrency: 32,
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
            tcp_probe_port: 3555,
            udp_probe_port: 3552,
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
            Ok(ConfigResponse {
                udp_probe_secret: vec![0u8; 8].into(),
                ..Default::default()
            })
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
            Ok(ConfigResponse {
                udp_probe_secret: vec![0u8; 8].into(),
                ..Default::default()
            })
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

        let runtime = AgentRuntime::bootstrap(test_env().await, api.clone(), cancel.clone())
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

        let runtime = AgentRuntime::bootstrap(test_env().await, api, cancel.clone())
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

        let mut runtime = AgentRuntime::bootstrap(test_env().await, api.clone(), cancel.clone())
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

        let runtime = AgentRuntime::bootstrap(test_env().await, api.clone(), cancel.clone())
            .await
            .expect("bootstrap should succeed after retries");

        assert!(
            api.call_count.load(Ordering::SeqCst) >= 3,
            "should have called register at least 3 times"
        );
        assert_eq!(runtime.supervisor_count(), 1);

        runtime.shutdown().await;
    }

    // -- HangThenSucceedApi --------------------------------------------------

    /// First `hang_count` calls to `register` hang forever (modelling a TCP
    /// connect that succeeds but the server never responds at the gRPC
    /// layer); subsequent calls succeed.
    struct HangThenSucceedApi {
        call_count: AtomicUsize,
        hang_count: usize,
    }

    impl HangThenSucceedApi {
        fn new(hang_count: usize) -> Arc<Self> {
            Arc::new(Self {
                call_count: AtomicUsize::new(0),
                hang_count,
            })
        }
    }

    impl ServiceApi for HangThenSucceedApi {
        async fn register(&self, _req: RegisterRequest) -> Result<RegisterResponse> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n < self.hang_count {
                std::future::pending::<()>().await;
            }
            Ok(RegisterResponse {})
        }

        async fn get_config(&self) -> Result<ConfigResponse> {
            Ok(ConfigResponse {
                udp_probe_secret: vec![0u8; 8].into(),
                ..Default::default()
            })
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

    // -- Test 5 --------------------------------------------------------------

    // -- Test 6 --------------------------------------------------------------

    /// If the configured TCP probe port is already in use, bootstrap must
    /// error out rather than registering the agent while silently running
    /// without a listener. Same principle for UDP (tested separately).
    #[tokio::test]
    async fn bootstrap_fails_when_tcp_port_in_use() {
        // Hold the port on `[::]` — same dual-stack address the bootstrap
        // binds on — for the duration of the test so the collision is
        // unambiguous across platforms. See the matching comment in
        // `probing::echo_tcp::tests::bind_fails_when_port_in_use`.
        let squatter = TcpListener::bind(("::", 0)).await.expect("squatter bind");
        let occupied_port = squatter.local_addr().unwrap().port();

        let mut env = test_env().await;
        env.tcp_probe_port = occupied_port;

        let api = MockApi::new(vec![test_target("peer-a")]);
        let res = AgentRuntime::bootstrap(env, api, CancellationToken::new()).await;
        let err = match res {
            Ok(_) => panic!("bootstrap should fail when TCP port is taken"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("tcp echo listener failed to bind"),
            "unexpected error: {msg}",
        );
        drop(squatter);
    }

    // -- BadConfigApi --------------------------------------------------------

    /// Returns a `ConfigResponse` whose `udp_probe_secret` is the wrong
    /// length (not 8 bytes). `ProbeConfig::from_proto` rejects this, and
    /// the bootstrap `?`-return fires AFTER the listener tasks are bound.
    struct BadConfigApi;

    impl ServiceApi for BadConfigApi {
        async fn register(&self, _req: RegisterRequest) -> Result<RegisterResponse> {
            Ok(RegisterResponse {})
        }

        async fn get_config(&self) -> Result<ConfigResponse> {
            Ok(ConfigResponse {
                udp_probe_secret: vec![0u8; 3].into(),
                ..Default::default()
            })
        }

        async fn get_targets(&self, _source_id: &str) -> Result<TargetsResponse> {
            Ok(TargetsResponse { targets: vec![] })
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

    /// Regression test: when bootstrap fails *after* the listener tasks have
    /// already bound their ports, the spawned tasks must be cancelled
    /// (via the internal drop-guard + child cancel token) so the ports are
    /// released and a subsequent attempt at the same ports can succeed.
    /// Prior to the fix, the listener tasks stayed up indefinitely.
    #[tokio::test]
    async fn bootstrap_releases_ports_on_late_failure() {
        let env = test_env().await;
        let tcp_port = env.tcp_probe_port;
        let udp_port = env.udp_probe_port;

        let api = Arc::new(BadConfigApi);
        let res = AgentRuntime::bootstrap(env, api, CancellationToken::new()).await;
        let err = match res {
            Ok(_) => panic!("bootstrap should fail on invalid ConfigResponse"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ConfigResponse") || msg.contains("udp_probe_secret"),
            "unexpected error: {msg}",
        );

        // The listener tasks should have been cancelled by the drop-guard.
        // Wait for them to release the ports, polling to avoid a flaky
        // scheduling race. Without the fix, this loop times out because
        // the listener tasks stay alive on the leaked cancel token.
        //
        // Rebind-check uses `[::]` since the real listeners bind
        // dual-stack — a `0.0.0.0` rebind would succeed even if the v6
        // side of the port is still held.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let tcp_free = TcpListener::bind(("::", tcp_port)).await.is_ok();
            let udp_free = UdpSocket::bind(("::", udp_port)).await.is_ok();
            if tcp_free && udp_free {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "listener ports still held after bootstrap failure \
                     (tcp_free={tcp_free}, udp_free={udp_free})"
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Same as above for UDP.
    #[tokio::test]
    async fn bootstrap_fails_when_udp_port_in_use() {
        // Hold the port on `[::]` — same dual-stack address the bootstrap
        // binds on — for the duration of the test so the collision is
        // unambiguous across platforms.
        let squatter = UdpSocket::bind(("::", 0)).await.expect("squatter bind");
        let occupied_port = squatter.local_addr().unwrap().port();

        let mut env = test_env().await;
        env.udp_probe_port = occupied_port;

        let api = MockApi::new(vec![test_target("peer-a")]);
        let res = AgentRuntime::bootstrap(env, api, CancellationToken::new()).await;
        let err = match res {
            Ok(_) => panic!("bootstrap should fail when UDP port is taken"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("udp echo listener failed to bind"),
            "unexpected error: {msg}",
        );
        drop(squatter);
    }

    // -- Test 7 --------------------------------------------------------------

    /// A hung RPC (TCP connected but server never responds) must be
    /// converted to a retryable error by the per-attempt timeout, so
    /// bootstrap eventually succeeds instead of stalling forever.
    #[tokio::test(start_paused = true)]
    async fn bootstrap_recovers_from_hung_rpc() {
        // First call to register hangs; second succeeds.
        let api = HangThenSucceedApi::new(1);
        let cancel = CancellationToken::new();

        let runtime = AgentRuntime::bootstrap(test_env().await, api.clone(), cancel.clone())
            .await
            .expect("bootstrap should succeed after timeout + retry");

        assert!(
            api.call_count.load(Ordering::SeqCst) >= 2,
            "register should have been called at least twice (hang, then succeed)"
        );
        assert_eq!(runtime.supervisor_count(), 1);

        runtime.shutdown().await;
    }

    /// A target registered with a 16-byte v4-mapped-v6 wire IP
    /// (`::ffff:a.b.c.d`) must land in the allowlist as its canonical
    /// `Ipv4Addr`, so the UDP echo listener — which canonicalizes each
    /// incoming peer address — finds it on lookup.
    #[tokio::test]
    async fn publish_allowlist_canonicalizes_v4_mapped_v6() {
        use std::net::Ipv4Addr;

        let (tx, rx) = watch::channel(Arc::new(HashSet::<IpAddr>::new()));
        // 16-byte wire: 10 zero bytes + 0xff 0xff + 4 v4 octets.
        let mut wire = vec![0u8; 10];
        wire.extend_from_slice(&[0xff, 0xff, 10, 0, 0, 1]);
        let target = Target {
            id: "peer-mapped".to_string(),
            ip: wire.into(),
            display_name: "Mapped".to_string(),
            location: "Test".to_string(),
            lat: 0.0,
            lon: 0.0,
            tcp_probe_port: 3555,
            udp_probe_port: 3552,
        };

        publish_allowlist(&tx, &[target]);

        let allowlist = rx.borrow().clone();
        assert!(
            allowlist.contains(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            "canonical v4 form must be in the allowlist; got {allowlist:?}",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_with_backoff_respects_custom_caps() {
        let cancel = CancellationToken::new();
        let counter = std::sync::atomic::AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let result: Result<()> = retry_with_backoff(
            || {
                let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    if n < 2 {
                        Err(anyhow::anyhow!("boom"))
                    } else {
                        Ok(())
                    }
                }
            },
            &cancel,
            "custom_caps_test",
            Duration::from_millis(100),
            Duration::from_millis(500),
        )
        .await;
        assert!(result.is_ok());
        // After 2 retryable failures: first sleep ~100ms*jitter, second ~200ms*jitter.
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(180)); // lower jitter bound
        assert!(elapsed < Duration::from_millis(500));
    }
}
