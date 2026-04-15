//! In-memory snapshot of the `agents` table.
//!
//! The registry keeps every registered agent — active and stale — in a
//! single `ArcSwap`-backed `RegistrySnapshot`. Callers choose the
//! staleness semantics at read time via [`RegistrySnapshot::active_targets`]
//! + a window taken from config.
//!
//! See the service README's "Agent registry" section for design context.

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// One `agents` row, decoupled from sqlx types so handlers can clone it
/// without borrowing the snapshot.
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
    /// Optional latitude.
    pub lat: Option<f64>,
    /// Optional longitude.
    pub lon: Option<f64>,
    /// Optional `agent_version` string reported on register.
    pub agent_version: Option<String>,
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
    lat: Option<f64>,
    lon: Option<f64>,
    agent_version: Option<String>,
    registered_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
}

/// Free function: read the current `agents` table into a snapshot.
async fn refresh_once(pool: &PgPool) -> Result<RegistrySnapshot, sqlx::Error> {
    let rows = sqlx::query_as!(
        AgentRow,
        r#"
        SELECT id, display_name, location,
               ip as "ip: IpNetwork",
               lat, lon, agent_version, registered_at, last_seen_at
        FROM agents
        "#,
    )
    .fetch_all(pool)
    .await?;

    let agents = rows
        .into_iter()
        .map(|row| AgentInfo {
            id: row.id,
            display_name: row.display_name,
            location: row.location,
            ip: row.ip,
            lat: row.lat,
            lon: row.lon,
            agent_version: row.agent_version,
            registered_at: row.registered_at,
            last_seen_at: row.last_seen_at,
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
}

impl AgentRegistry {
    /// Construct an empty registry. No DB contact until [`AgentRegistry::initial_load`] or
    /// [`AgentRegistry::force_refresh`] is called.
    pub fn new(pool: PgPool, refresh_interval: Duration) -> Self {
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(RegistrySnapshot::empty())),
            pool,
            refresh_interval,
        }
    }

    /// Perform the first DB read synchronously. Called during service
    /// startup and expected to succeed; a failure here is fail-fast
    /// because a cold registry would reject every subsequent ingestion as
    /// unknown-source (403).
    pub async fn initial_load(&self) -> Result<(), sqlx::Error> {
        let snap = refresh_once(&self.pool).await?;
        self.snapshot.store(Arc::new(snap));
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

    /// Refresh right now and await completion. Called by
    /// `POST /api/agent/register` after writing the new row, so the
    /// caller's next request sees it without waiting for the next tick.
    ///
    /// Last-writer-wins semantics vs. the periodic loop: two concurrent
    /// refreshes both succeed; the `store` that arrives later overwrites
    /// the earlier one. At ~40 agents and 10s cadence this is benign.
    pub async fn force_refresh(&self) -> Result<(), sqlx::Error> {
        let snap = refresh_once(&self.pool).await?;
        self.snapshot.store(Arc::new(snap));
        Ok(())
    }
}
