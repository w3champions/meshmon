//! User-facing agent endpoints.
//!
//! - `GET /api/agents` — list every agent in the registry snapshot.
//!
//! All endpoints sit behind the `login_required!` layer, so
//! unauthenticated callers receive 401 from the middleware before the
//! handler runs.

use crate::registry::AgentInfo;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

/// Summary of a single agent, returned by the list and detail endpoints.
///
/// Write-only on the server (constructed and serialized, never parsed) so
/// only `Serialize` is derived.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentSummary {
    /// Unique agent identifier (matches the agent's `AGENT_ID` env var).
    pub id: String,
    /// Human-readable display label.
    pub display_name: String,
    /// Optional free-form location string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Agent source IP (host address only, CIDR prefix stripped).
    pub ip: String,
    /// Optional latitude.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    /// Optional longitude.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    /// Optional agent version string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    /// When this agent first registered.
    pub registered_at: chrono::DateTime<chrono::Utc>,
    /// Last successful push (register/metrics/snapshot).
    pub last_seen_at: chrono::DateTime<chrono::Utc>,
}

impl From<AgentInfo> for AgentSummary {
    fn from(a: AgentInfo) -> Self {
        Self {
            id: a.id,
            display_name: a.display_name,
            location: a.location,
            ip: a.ip.ip().to_string(),
            lat: a.lat,
            lon: a.lon,
            agent_version: a.agent_version,
            registered_at: a.registered_at,
            last_seen_at: a.last_seen_at,
        }
    }
}

/// `GET /api/agents` — return every agent known to the registry.
///
/// The response is a flat JSON array sorted by `id` for determinism.
/// Empty when no agents have registered yet.
#[utoipa::path(
    get,
    path = "/api/agents",
    tag = "agents",
    responses(
        (status = 200, description = "List of all registered agents", body = Vec<AgentSummary>),
        (status = 401, description = "No active session"),
    ),
)]
pub async fn list_agents(State(state): State<AppState>) -> Json<Vec<AgentSummary>> {
    let snap = state.registry.snapshot();
    let mut agents: Vec<AgentSummary> = snap.all().into_iter().map(AgentSummary::from).collect();
    agents.sort_by(|a, b| a.id.cmp(&b.id));
    Json(agents)
}

