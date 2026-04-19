//! In-memory snapshot of the `agents` table.
//!
//! The registry keeps every registered agent — active and stale — in a
//! single `ArcSwap`-backed `RegistrySnapshot`. Callers choose the
//! staleness semantics at read time via [`RegistrySnapshot::active_targets`]
//! + a window taken from config.
//!
//! See the service README's "Agent registry" section for design context.

use crate::metrics::{
    registry_agents, registry_last_refresh_age_seconds, registry_refresh_errors, AgentState,
};
use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// One `agents` row, decoupled from sqlx types so handlers can clone it
/// without borrowing the snapshot. Geo coordinates are joined in from the
/// IP catalogue — the `agents` table itself no longer carries them.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    /// Human-readable identifier (matches the agent's `AGENT_ID` env var).
    pub id: String,
    /// Display label shown in the UI.
    pub display_name: String,
    /// Optional free-form location string.
    pub location: Option<String>,
    /// Source IP (CIDR-wrapped by Postgres' `INET` type).
    pub ip: IpNetwork,
    /// Latitude joined from `ip_catalogue`, if present.
    pub latitude: Option<f64>,
    /// Longitude joined from `ip_catalogue`, if present.
    pub longitude: Option<f64>,
    /// Optional `agent_version` string reported on register.
    pub agent_version: Option<String>,
    /// Advertised TCP echo-listener port (1-65535, enforced by DB CHECK).
    pub tcp_probe_port: u16,
    /// Advertised UDP echo-listener port (1-65535, enforced by DB CHECK).
    pub udp_probe_port: u16,
    /// When this agent first registered.
    pub registered_at: DateTime<Utc>,
    /// Last successful push (register/metrics/snapshot).
    pub last_seen_at: DateTime<Utc>,
}

/// Immutable snapshot of the registry.
///
/// Built by `refresh_once` and stored inside `AgentRegistry` under an
/// `ArcSwap` for lock-free concurrent reads.
#[derive(Debug, Clone)]
pub struct RegistrySnapshot {
    agents: HashMap<String, AgentInfo>,
    refreshed_at: DateTime<Utc>,
}

impl RegistrySnapshot {
    /// Empty snapshot marked as refreshed "now". Used as the seed value
    /// inside `ArcSwap` before the first successful DB load.
    pub fn empty() -> Self {
        Self {
            agents: HashMap::new(),
            refreshed_at: Utc::now(),
        }
    }

    /// Build a snapshot from a vector of rows. Used by `refresh_once` after
    /// a successful DB read and by tests that seed a mock snapshot.
    pub fn from_agents(agents: Vec<AgentInfo>) -> Self {
        let mut map = HashMap::with_capacity(agents.len());
        for a in agents {
            map.insert(a.id.clone(), a);
        }
        Self {
            agents: map,
            refreshed_at: Utc::now(),
        }
    }

    /// When this snapshot was built.
    pub fn refreshed_at(&self) -> DateTime<Utc> {
        self.refreshed_at
    }

    /// Number of agents in the snapshot.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// Snapshot contains no agents.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Look up one agent by id.
    pub fn get(&self, agent_id: &str) -> Option<&AgentInfo> {
        self.agents.get(agent_id)
    }

    /// Clone every agent out for listing endpoints.
    pub fn all(&self) -> Vec<AgentInfo> {
        self.agents.values().cloned().collect()
    }

    /// Unix-epoch seconds of `last_seen_at`, if the agent is known.
    pub fn last_seen_seconds(&self, agent_id: &str) -> Option<i64> {
        self.agents
            .get(agent_id)
            .map(|a| a.last_seen_at.timestamp())
    }

    /// Agents eligible to appear in a peer's `/api/agent/targets` list.
    ///
    /// An agent is active iff `last_seen_at > now - window`. `excluding` is
    /// removed from the result (an agent doesn't probe itself). `now` is
    /// sampled at call time so the window stays accurate between refreshes.
    pub fn active_targets(&self, excluding: &str, window: Duration) -> Vec<AgentInfo> {
        let cutoff =
            Utc::now() - chrono::Duration::from_std(window).unwrap_or(chrono::Duration::MAX);
        self.agents
            .values()
            .filter(|a| a.id != excluding && a.last_seen_at > cutoff)
            .cloned()
            .collect()
    }
}

/// Flat row struct for `sqlx::query_as!`. Kept private to the module.
struct AgentRow {
    id: String,
    display_name: String,
    location: Option<String>,
    ip: IpNetwork,
    latitude: Option<f64>,
    longitude: Option<f64>,
    agent_version: Option<String>,
    tcp_probe_port: i32,
    udp_probe_port: i32,
    registered_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
}

/// Free function: read the current `agents` table (joined with the IP
/// catalogue for geo) into a snapshot.
async fn refresh_once(pool: &PgPool) -> Result<RegistrySnapshot, sqlx::Error> {
    let rows = sqlx::query_as!(
        AgentRow,
        r#"
        SELECT a.id, a.display_name, a.location,
               a.ip AS "ip: IpNetwork",
               c.latitude, c.longitude,
               a.agent_version,
               a.tcp_probe_port, a.udp_probe_port,
               a.registered_at, a.last_seen_at
        FROM agents a
        LEFT JOIN ip_catalogue c ON c.ip = a.ip
        "#,
    )
    .fetch_all(pool)
    .await?;

    // DB has CHECK (port BETWEEN 1 AND 65535) on both columns, so the cast
    // below always succeeds on a well-formed database. We still guard here:
    // a constraint drift (`ALTER TABLE ... NOT VALID`, replica skew, manual
    // tamper) would otherwise panic the refresh task, kill the `JoinHandle`
    // silently, and leave the registry stale indefinitely. Log and skip
    // instead so one bad row doesn't sink the whole snapshot.
    let agents: Vec<AgentInfo> = rows
        .into_iter()
        .filter_map(|row| {
            let tcp_probe_port = match u16::try_from(row.tcp_probe_port) {
                Ok(v) if v > 0 => v,
                _ => {
                    tracing::error!(
                        agent_id = %row.id,
                        value = row.tcp_probe_port,
                        "skipping agent with out-of-range tcp_probe_port",
                    );
                    return None;
                }
            };
            let udp_probe_port = match u16::try_from(row.udp_probe_port) {
                Ok(v) if v > 0 => v,
                _ => {
                    tracing::error!(
                        agent_id = %row.id,
                        value = row.udp_probe_port,
                        "skipping agent with out-of-range udp_probe_port",
                    );
                    return None;
                }
            };
            Some(AgentInfo {
                id: row.id,
                display_name: row.display_name,
                location: row.location,
                ip: row.ip,
                latitude: row.latitude,
                longitude: row.longitude,
                agent_version: row.agent_version,
                tcp_probe_port,
                udp_probe_port,
                registered_at: row.registered_at,
                last_seen_at: row.last_seen_at,
            })
        })
        .collect();

    Ok(RegistrySnapshot::from_agents(agents))
}

/// Test seam: exposes [`refresh_once`] to integration tests, which live in
/// a separate crate and therefore cannot access private items.
#[doc(hidden)]
pub async fn refresh_once_for_test(pool: &PgPool) -> Result<RegistrySnapshot, sqlx::Error> {
    refresh_once(pool).await
}

/// Owning wrapper around an `Arc<ArcSwap<RegistrySnapshot>>` plus the pool
/// and refresh cadence.
///
/// `AgentRegistry` is constructed once at startup, held inside `AppState`,
/// and cloned via `Arc` by every handler that needs registry access.
pub struct AgentRegistry {
    snapshot: Arc<ArcSwap<RegistrySnapshot>>,
    pool: PgPool,
    refresh_interval: Duration,
    active_window: Duration,
}

impl AgentRegistry {
    /// Construct an empty registry. No DB contact until [`AgentRegistry::initial_load`] or
    /// [`AgentRegistry::force_refresh`] is called.
    ///
    /// `active_window` is the lookback duration used when splitting agents
    /// into `active` vs `stale` for the `agents{state}` gauge.
    pub fn new(pool: PgPool, refresh_interval: Duration, active_window: Duration) -> Self {
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(RegistrySnapshot::empty())),
            pool,
            refresh_interval,
            active_window,
        }
    }

    /// The staleness window passed at construction time.
    pub fn active_window(&self) -> Duration {
        self.active_window
    }

    /// Perform the first DB read synchronously. Called during service
    /// startup and expected to succeed; a failure here is fail-fast
    /// because a cold registry would reject every subsequent ingestion as
    /// unknown-source (403).
    pub async fn initial_load(&self) -> Result<(), sqlx::Error> {
        let snap = refresh_once(&self.pool).await?;
        self.publish(&snap);
        self.snapshot.store(Arc::new(snap));
        // Seed the refresh-age gauge so operators get an honest reading
        // even before the periodic loop ticks. Without this, the gauge
        // is absent (or stuck at 0) for up to `refresh_interval` after
        // startup, which reads as "snapshot is fresh" when it's really
        // "we haven't measured yet".
        registry_last_refresh_age_seconds().set(0.0);
        Ok(())
    }

    /// Lock-free read of the current snapshot.
    pub fn snapshot(&self) -> Arc<RegistrySnapshot> {
        self.snapshot.load_full()
    }

    /// How often the periodic refresh loop wakes up.
    pub fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }

    /// Refresh right now and await completion. Intended for the
    /// agent-register handler to invoke after writing a new row, so the
    /// caller's next request sees the agent without waiting for the next
    /// periodic tick.
    ///
    /// Last-writer-wins semantics vs. the periodic loop: two concurrent
    /// refreshes both succeed; the `store` that arrives later overwrites
    /// the earlier one. At ~40 agents and 10s cadence this is benign.
    pub async fn force_refresh(&self) -> Result<(), sqlx::Error> {
        let snap = refresh_once(&self.pool).await?;
        self.publish(&snap);
        self.snapshot.store(Arc::new(snap));
        // Keep the refresh-age gauge aligned with the snapshot just
        // published — the periodic loop also resets it, but a force
        // refresh between ticks should not leave the gauge stale.
        registry_last_refresh_age_seconds().set(0.0);
        Ok(())
    }

    /// Emit the `meshmon_service_registry_agents` gauge split by
    /// `state` label (`"active"` / `"stale"`).
    fn publish(&self, snap: &RegistrySnapshot) {
        let window = self.active_window;
        let cutoff =
            Utc::now() - chrono::Duration::from_std(window).unwrap_or(chrono::Duration::MAX);
        let mut active = 0u64;
        let mut stale = 0u64;
        for a in snap.agents.values() {
            if a.last_seen_at > cutoff {
                active += 1;
            } else {
                stale += 1;
            }
        }
        registry_agents(AgentState::Active).set(active as f64);
        registry_agents(AgentState::Stale).set(stale as f64);
    }

    /// Spawn the periodic refresh task.
    ///
    /// The loop wakes every `refresh_interval`, calls [`refresh_once`],
    /// and — on success — atomically swaps the new snapshot into place.
    /// On failure it logs at `warn`, increments
    /// `meshmon_service_registry_refresh_errors_total`, and leaves the
    /// prior snapshot untouched. Stale data is strictly preferable to a
    /// cold registry.
    ///
    /// `token` cancels the loop. One in-flight refresh may complete after
    /// `token` is cancelled, since cancellation is checked only after
    /// `refresh_once` returns. The extra query is bounded by sqlx's connect
    /// timeout and is harmless. The returned `JoinHandle<()>` must be
    /// awaited during shutdown (see `main.rs`).
    pub fn spawn_refresh(
        self: Arc<Self>,
        token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let interval = self.refresh_interval;
            loop {
                match refresh_once(&self.pool).await {
                    Ok(snap) => {
                        tracing::debug!(count = snap.len(), "agent registry refreshed",);
                        self.publish(&snap);
                        self.snapshot.store(Arc::new(snap));
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "agent registry refresh failed; keeping stale snapshot",
                        );
                        registry_refresh_errors().increment(1);
                    }
                }

                // Reflect the snapshot's current age after this iteration: ~0
                // on success, otherwise (prior age + iteration cost).
                let age_secs = (Utc::now() - self.snapshot().refreshed_at())
                    .num_seconds()
                    .max(0) as f64;
                registry_last_refresh_age_seconds().set(age_secs);

                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        tracing::debug!("agent registry refresh loop exiting");
                        return;
                    }
                    _ = tokio::time::sleep(interval) => {}
                }
            }
        })
    }
}
