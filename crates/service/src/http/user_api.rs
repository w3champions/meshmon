//! User-facing agent and route-snapshot endpoints.
//!
//! - `GET /api/agents` — list every agent in the registry snapshot.
//! - `GET /api/agents/{id}` — single-agent detail by id.
//! - `GET /api/paths/{src}/{tgt}/routes/latest` — most recent route snapshot.
//! - `GET /api/paths/{src}/{tgt}/routes/{snapshot_id}` — snapshot by id.
//! - `GET /api/paths/{src}/{tgt}/routes` — paginated route snapshot list.
//!
//! All endpoints sit behind the `login_required!` layer, so
//! unauthenticated callers receive 401 from the middleware before the
//! handler runs.

use crate::ingestion::json_shapes::{HopJson, PathSummaryJson};
use crate::registry::AgentInfo;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::Json as SqlxJson;
use utoipa::{IntoParams, ToSchema};

/// Latitude / longitude pair sourced from the IP catalogue join.
///
/// Present only when both coordinates are known; otherwise the parent
/// `AgentSummary.catalogue_coordinates` is `None`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CatalogueCoordinates {
    /// Decimal latitude (-90..=90).
    pub latitude: f64,
    /// Decimal longitude (-180..=180).
    pub longitude: f64,
}

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
    /// Geo coordinates joined from the IP catalogue. Absent when the
    /// agent's IP is not in the catalogue or neither coord is populated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalogue_coordinates: Option<CatalogueCoordinates>,
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
        let catalogue_coordinates = match (a.latitude, a.longitude) {
            (Some(latitude), Some(longitude)) => Some(CatalogueCoordinates {
                latitude,
                longitude,
            }),
            _ => None,
        };
        Self {
            id: a.id,
            display_name: a.display_name,
            location: a.location,
            ip: a.ip.ip().to_string(),
            catalogue_coordinates,
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

/// `GET /api/agents/{id}` — return a single agent by id.
///
/// Returns 404 with a JSON error body when the id is not found in the
/// current registry snapshot.
#[utoipa::path(
    get,
    path = "/api/agents/{id}",
    tag = "agents",
    params(
        ("id" = String, Path, description = "Agent identifier"),
    ),
    responses(
        (status = 200, description = "Agent detail", body = AgentSummary),
        (status = 401, description = "No active session"),
        (status = 404, description = "Agent not found"),
    ),
)]
pub async fn get_agent(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let snap = state.registry.snapshot();
    match snap.get(&id).cloned() {
        Some(info) => Json(AgentSummary::from(info)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "agent not found" })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Route-snapshot DTOs
// ---------------------------------------------------------------------------

/// Full detail of a single route snapshot (includes hops).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RouteSnapshotDetail {
    /// Database row id.
    pub id: i64,
    /// Source agent identifier.
    pub source_id: String,
    /// Target agent identifier.
    pub target_id: String,
    /// Protocol used for the traceroute (icmp, tcp, udp).
    pub protocol: String,
    /// When the snapshot was observed.
    pub observed_at: DateTime<Utc>,
    /// Hop-by-hop detail.
    pub hops: Vec<HopJson>,
    /// Aggregated path summary (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_summary: Option<PathSummaryJson>,
}

/// Summary of a route snapshot (no hops — used in list responses).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RouteSnapshotSummary {
    /// Database row id.
    pub id: i64,
    /// Source agent identifier.
    pub source_id: String,
    /// Target agent identifier.
    pub target_id: String,
    /// Protocol used for the traceroute (icmp, tcp, udp).
    pub protocol: String,
    /// When the snapshot was observed.
    pub observed_at: DateTime<Utc>,
    /// Aggregated path summary (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_summary: Option<PathSummaryJson>,
}

/// Query parameter for protocol selection (defaults to `icmp`).
#[derive(Debug, Deserialize, IntoParams)]
pub struct ProtocolQuery {
    /// Protocol filter: icmp, tcp, or udp.
    #[serde(default = "default_protocol")]
    #[param(default = "icmp")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "icmp".to_string()
}

/// Validate protocol value; returns `None` on valid input or `Some(response)`
/// with a 400 error body on invalid input.
///
/// Shared with `path_overview::path_overview` — keep this the single source
/// of truth so the error shape stays identical across `/api/paths/...`
/// endpoints.
pub(crate) fn invalid_protocol(protocol: &str) -> Option<Response> {
    match protocol {
        "icmp" | "tcp" | "udp" => None,
        _ => Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "protocol must be icmp, tcp, or udp" })),
            )
                .into_response(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Task 5: GET /api/paths/{src}/{tgt}/routes/latest
// ---------------------------------------------------------------------------

/// Fetch the most recent route snapshot for a source/target/protocol triple.
async fn fetch_latest(
    pool: &sqlx::PgPool,
    source_id: &str,
    target_id: &str,
    protocol: &str,
) -> Result<Option<RouteSnapshotDetail>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT id AS "id!",
                  source_id,
                  target_id,
                  protocol,
                  observed_at,
                  hops AS "hops: SqlxJson<Vec<HopJson>>",
                  path_summary AS "path_summary: SqlxJson<PathSummaryJson>"
           FROM route_snapshots
           WHERE source_id = $1
             AND target_id = $2
             AND protocol   = $3
           ORDER BY observed_at DESC
           LIMIT 1"#,
        source_id,
        target_id,
        protocol,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| RouteSnapshotDetail {
        id: r.id,
        source_id: r.source_id,
        target_id: r.target_id,
        protocol: r.protocol,
        observed_at: r.observed_at,
        hops: r.hops.0,
        path_summary: r.path_summary.map(|s| s.0),
    }))
}

/// `GET /api/paths/{src}/{tgt}/routes/latest` — return the most recent
/// route snapshot for the given source, target, and protocol.
#[utoipa::path(
    get,
    path = "/api/paths/{src}/{tgt}/routes/latest",
    tag = "routes",
    params(
        ("src" = String, Path, description = "Source agent id"),
        ("tgt" = String, Path, description = "Target agent id"),
        ProtocolQuery,
    ),
    responses(
        (status = 200, description = "Latest route snapshot", body = RouteSnapshotDetail),
        (status = 400, description = "Invalid protocol"),
        (status = 401, description = "No active session"),
        (status = 404, description = "No snapshot found"),
    ),
)]
pub async fn get_route_latest(
    State(state): State<AppState>,
    Path((src, tgt)): Path<(String, String)>,
    Query(q): Query<ProtocolQuery>,
) -> Response {
    if let Some(resp) = invalid_protocol(&q.protocol) {
        return resp;
    }
    match fetch_latest(&state.pool, &src, &tgt, &q.protocol).await {
        Ok(Some(detail)) => Json(detail).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no snapshot found" })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "fetch_latest failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Task 6: GET /api/paths/{src}/{tgt}/routes/{snapshot_id}
// ---------------------------------------------------------------------------

/// Fetch a single route snapshot by id, scoped to the given source/target.
async fn fetch_by_id(
    pool: &sqlx::PgPool,
    snapshot_id: i64,
    source_id: &str,
    target_id: &str,
) -> Result<Option<RouteSnapshotDetail>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT id AS "id!",
                  source_id,
                  target_id,
                  protocol,
                  observed_at,
                  hops AS "hops: SqlxJson<Vec<HopJson>>",
                  path_summary AS "path_summary: SqlxJson<PathSummaryJson>"
           FROM route_snapshots
           WHERE id        = $1
             AND source_id = $2
             AND target_id = $3"#,
        snapshot_id,
        source_id,
        target_id,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| RouteSnapshotDetail {
        id: r.id,
        source_id: r.source_id,
        target_id: r.target_id,
        protocol: r.protocol,
        observed_at: r.observed_at,
        hops: r.hops.0,
        path_summary: r.path_summary.map(|s| s.0),
    }))
}

/// `GET /api/paths/{src}/{tgt}/routes/{snapshot_id}` — return a single
/// route snapshot by database id.
#[utoipa::path(
    get,
    path = "/api/paths/{src}/{tgt}/routes/{snapshot_id}",
    tag = "routes",
    params(
        ("src" = String, Path, description = "Source agent id"),
        ("tgt" = String, Path, description = "Target agent id"),
        ("snapshot_id" = i64, Path, description = "Snapshot database id"),
    ),
    responses(
        (status = 200, description = "Route snapshot detail", body = RouteSnapshotDetail),
        (status = 401, description = "No active session"),
        (status = 404, description = "Snapshot not found"),
    ),
)]
pub async fn get_route_by_id(
    State(state): State<AppState>,
    Path((src, tgt, snapshot_id)): Path<(String, String, i64)>,
) -> Response {
    match fetch_by_id(&state.pool, snapshot_id, &src, &tgt).await {
        Ok(Some(detail)) => Json(detail).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "snapshot not found" })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "fetch_by_id failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Task 7: GET /api/paths/{src}/{tgt}/routes — paginated list
// ---------------------------------------------------------------------------

/// Maximum allowed `limit` parameter.
const ROUTES_LIST_MAX_LIMIT: i64 = 500;
/// Default `limit` when not specified.
const ROUTES_LIST_DEFAULT_LIMIT: i64 = 100;

/// Paginated response of route snapshot summaries.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RoutesPage {
    /// Snapshot summaries in descending `observed_at` order.
    pub items: Vec<RouteSnapshotSummary>,
    /// Current offset.
    pub offset: i64,
    /// Applied limit.
    pub limit: i64,
}

/// Query parameters for the paginated routes list.
#[derive(Debug, Deserialize, IntoParams)]
pub struct RoutesListParams {
    /// Start of the time window (inclusive). Defaults to 24 h ago.
    pub from: Option<DateTime<Utc>>,
    /// End of the time window (inclusive). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Maximum rows to return (1..=500, default 100).
    pub limit: Option<i64>,
    /// Offset for pagination (>= 0, default 0).
    pub offset: Option<i64>,
    /// Protocol filter: icmp, tcp, or udp.
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

/// Resolved pagination and filter parameters for the list query.
struct ListFilter<'a> {
    source_id: &'a str,
    target_id: &'a str,
    protocol: &'a str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: i64,
    offset: i64,
}

/// Fetch a paginated list of route snapshot summaries (no hops).
async fn fetch_list(
    pool: &sqlx::PgPool,
    f: &ListFilter<'_>,
) -> Result<Vec<RouteSnapshotSummary>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT id AS "id!",
                  source_id,
                  target_id,
                  protocol,
                  observed_at,
                  path_summary AS "path_summary: SqlxJson<PathSummaryJson>"
           FROM route_snapshots
           WHERE source_id   = $1
             AND target_id   = $2
             AND protocol    = $3
             AND observed_at >= $4
             AND observed_at <= $5
           ORDER BY observed_at DESC
           LIMIT $6
           OFFSET $7"#,
        f.source_id,
        f.target_id,
        f.protocol,
        f.from,
        f.to,
        f.limit,
        f.offset,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| RouteSnapshotSummary {
            id: r.id,
            source_id: r.source_id,
            target_id: r.target_id,
            protocol: r.protocol,
            observed_at: r.observed_at,
            path_summary: r.path_summary.map(|s| s.0),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// T18: GET /api/routes/recent — cross-pair recent snapshots
// ---------------------------------------------------------------------------

const RECENT_ROUTES_MAX_LIMIT: i64 = 100;
const RECENT_ROUTES_DEFAULT_LIMIT: i64 = 10;

/// Query parameters for the recent-routes endpoint.
#[derive(Debug, Deserialize, IntoParams)]
pub struct RecentRoutesParams {
    /// Maximum rows to return (1..=100, default 10).
    pub limit: Option<i64>,
}

/// Fetch the most recent route snapshots across all source/target pairs.
///
/// Uses a CTE with `DISTINCT ON` to return exactly one (the latest) snapshot
/// per `(source_id, target_id)` pair, then orders the result set globally by
/// `observed_at DESC` so the caller sees the most recently active pairs first.
///
/// Protocol is collapsed: when a pair has multiple protocols (e.g. icmp + tcp),
/// only the snapshot with the newest `observed_at` across protocols is kept.
/// The per-protocol route history remains available via `/api/paths/{src}/{tgt}/routes`.
async fn fetch_recent_routes(
    pool: &sqlx::PgPool,
    limit: i64,
) -> Result<Vec<RouteSnapshotSummary>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"WITH latest AS (
               SELECT DISTINCT ON (source_id, target_id)
                      id, source_id, target_id, protocol, observed_at, path_summary
               FROM route_snapshots
               ORDER BY source_id, target_id, observed_at DESC, id DESC
           )
           SELECT id AS "id!",
                  source_id,
                  target_id,
                  protocol,
                  observed_at,
                  path_summary AS "path_summary: SqlxJson<PathSummaryJson>"
           FROM latest
           ORDER BY observed_at DESC, id DESC
           LIMIT $1"#,
        limit,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| RouteSnapshotSummary {
            id: r.id,
            source_id: r.source_id,
            target_id: r.target_id,
            protocol: r.protocol,
            observed_at: r.observed_at,
            path_summary: r.path_summary.map(|s| s.0),
        })
        .collect())
}

/// `GET /api/routes/recent` — returns the latest route snapshot per
/// `(source_id, target_id)` pair, newest first, up to `limit` rows
/// (default 10, max 100).
#[utoipa::path(
    get,
    path = "/api/routes/recent",
    tag = "routes",
    params(RecentRoutesParams),
    responses(
        (status = 200, description = "Recent route snapshots across all pairs", body = Vec<RouteSnapshotSummary>),
        (status = 400, description = "Invalid limit"),
        (status = 401, description = "No active session"),
    ),
)]
pub async fn list_recent_routes(
    State(state): State<AppState>,
    Query(q): Query<RecentRoutesParams>,
) -> Response {
    let limit = q.limit.unwrap_or(RECENT_ROUTES_DEFAULT_LIMIT);
    if !(1..=RECENT_ROUTES_MAX_LIMIT).contains(&limit) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("limit must be between 1 and {RECENT_ROUTES_MAX_LIMIT}")
            })),
        )
            .into_response();
    }

    match fetch_recent_routes(&state.pool, limit).await {
        Ok(items) => Json(items).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "fetch_recent_routes failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET /api/paths/{src}/{tgt}/routes` — return a paginated, time-filtered
/// list of route snapshot summaries (without hop detail).
#[utoipa::path(
    get,
    path = "/api/paths/{src}/{tgt}/routes",
    tag = "routes",
    params(
        ("src" = String, Path, description = "Source agent id"),
        ("tgt" = String, Path, description = "Target agent id"),
        RoutesListParams,
    ),
    responses(
        (status = 200, description = "Paginated route snapshots", body = RoutesPage),
        (status = 400, description = "Invalid parameters"),
        (status = 401, description = "No active session"),
    ),
)]
pub async fn list_routes(
    State(state): State<AppState>,
    Path((src, tgt)): Path<(String, String)>,
    Query(q): Query<RoutesListParams>,
) -> Response {
    if let Some(resp) = invalid_protocol(&q.protocol) {
        return resp;
    }

    let limit = q.limit.unwrap_or(ROUTES_LIST_DEFAULT_LIMIT);
    if !(1..=ROUTES_LIST_MAX_LIMIT).contains(&limit) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("limit must be between 1 and {ROUTES_LIST_MAX_LIMIT}")
            })),
        )
            .into_response();
    }

    let offset = q.offset.unwrap_or(0);
    if offset < 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "offset must be >= 0" })),
        )
            .into_response();
    }

    let now = Utc::now();
    let to = q.to.unwrap_or(now);
    let from = q.from.unwrap_or(to - chrono::Duration::hours(24));

    if from > to {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "from must be <= to" })),
        )
            .into_response();
    }

    let filter = ListFilter {
        source_id: &src,
        target_id: &tgt,
        protocol: &q.protocol,
        from,
        to,
        limit,
        offset,
    };

    match fetch_list(&state.pool, &filter).await {
        Ok(items) => Json(RoutesPage {
            items,
            offset,
            limit,
        })
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "fetch_list failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
