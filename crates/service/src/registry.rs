//! In-memory snapshot of the `agents` table.
//!
//! The registry keeps every registered agent — active and stale — in a
//! single `ArcSwap`-backed `RegistrySnapshot`. Callers choose the
//! staleness semantics at read time via [`RegistrySnapshot::active_targets`]
//! + a window taken from config.
//!
//! See `docs/superpowers/specs/2026-04-13-meshmon-03-central-service.md`
//! (§Agent registry) for the design.

use chrono::{DateTime, Utc};
use sqlx::types::ipnetwork::IpNetwork;
use std::collections::HashMap;
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
