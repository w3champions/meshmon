//! `/api/history/*` endpoints.
//!
//! Discovery surfaces for the historic pair view (spec 04 §6):
//! - `GET /api/history/sources` — agents that appear as `source_agent_id`
//!   in any `measurements` row.
//! - `GET /api/history/destinations` — every `destination_ip` with ≥ 1
//!   measurement from the chosen source.
//! - `GET /api/history/measurements` — measurements (+ joined mtr_traces)
//!   for a (source, destination) over a time range.
//!
//! A fourth endpoint `GET /api/campaigns/{id}/measurements` (T49 addition)
//! feeds the Results browser's Raw tab — it lives here for locality with
//! the measurements-attribution SQL.
//!
//! Auth is inherited from the user-API middleware layer; handlers do not
//! take an `AuthSession` extractor.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::campaign::dto::ErrorEnvelope;
use crate::state::AppState;

// All history handlers use `&state.pool` (PgPool is a public field on
// AppState — there is no `pg_pool()` accessor). Auth is inherited from
// the user-API middleware; handlers do not take an `AuthSession` extractor.

/// Shared error mapper for history handlers — all failures collapse to 500.
fn internal_error(scope: &str, err: sqlx::Error) -> Response {
    tracing::error!(scope, error = %err, "history db error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorEnvelope {
            error: "internal".into(),
        }),
    )
        .into_response()
}

// --- sources -----------------------------------------------------------

/// One entry in the `/api/history/sources` list.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct HistorySourceDto {
    /// Agent id.
    pub source_agent_id: String,
    /// Display name from `agents_with_catalogue` — the catalogue-derived
    /// name when set, else the agent's own `display_name`, else the id.
    pub display_name: String,
}

/// `GET /api/history/sources` — agents with at least one measurement.
#[utoipa::path(
    get,
    path = "/api/history/sources",
    tag = "history",
    operation_id = "history_sources",
    responses(
        (status = 200, description = "Source list", body = Vec<HistorySourceDto>),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn sources(State(state): State<AppState>) -> Response {
    let pool = &state.pool;
    match sqlx::query_as!(
        HistorySourceDto,
        r#"
        SELECT DISTINCT
               awc.agent_id AS "source_agent_id!",
               COALESCE(awc.catalogue_display_name, awc.agent_display_name, awc.agent_id)
                 AS "display_name!"
          FROM agents_with_catalogue awc
          JOIN measurements m ON m.source_agent_id = awc.agent_id
         ORDER BY 2 ASC
        "#,
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => internal_error("history::sources", e),
    }
}

// --- destinations ------------------------------------------------------

/// Query params for `GET /api/history/destinations`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct HistoryDestinationsQuery {
    /// Source agent id — required.
    pub source: String,
    /// Partial match on `destination_ip` or catalogue `display_name`.
    #[serde(default)]
    pub q: Option<String>,
}

/// One destination reachable from a given source.
///
/// `display_name` falls back to `host(destination_ip)` when no catalogue
/// row exists (either never enriched or later deleted). The frontend
/// renders this as "raw IP — no metadata", a supported state rather than
/// a rendering bug — `city`, `country_code`, and `asn` all stay NULL.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct HistoryDestinationDto {
    /// Raw destination IP as a host string.
    pub destination_ip: String,
    /// Catalogue-derived label when known, else the IP string.
    pub display_name: String,
    /// Catalogue city (nullable).
    pub city: Option<String>,
    /// Catalogue country code (nullable).
    pub country_code: Option<String>,
    /// Catalogue ASN (nullable). Postgres `INTEGER` → `i32`.
    pub asn: Option<i32>,
    /// Whether the destination IP is itself a mesh-agent IP.
    pub is_mesh_member: bool,
}

/// `GET /api/history/destinations` — every destination reachable from
/// `source`, optionally narrowed by partial match on IP or display
/// name (case-insensitive).
#[utoipa::path(
    get,
    path = "/api/history/destinations",
    tag = "history",
    operation_id = "history_destinations",
    params(HistoryDestinationsQuery),
    responses(
        (status = 200, description = "Destination list", body = Vec<HistoryDestinationDto>),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn destinations(
    State(state): State<AppState>,
    Query(q): Query<HistoryDestinationsQuery>,
) -> Response {
    let pool = &state.pool;
    let pattern = q.q.as_deref().map(|s| format!("%{}%", s.to_lowercase()));

    match sqlx::query_as!(
        HistoryDestinationDto,
        r#"
        SELECT
          host(m.destination_ip)                           AS "destination_ip!",
          COALESCE(c.display_name, host(m.destination_ip)) AS "display_name!",
          c.city,
          c.country_code,
          c.asn,
          EXISTS (SELECT 1 FROM agents a WHERE a.ip = m.destination_ip)
                                                            AS "is_mesh_member!"
        FROM (
          SELECT DISTINCT destination_ip
            FROM measurements
           WHERE source_agent_id = $1
        ) m
        LEFT JOIN ip_catalogue c ON c.ip = m.destination_ip
        WHERE $2::text IS NULL
           OR LOWER(host(m.destination_ip)) LIKE $2
           OR (c.display_name IS NOT NULL AND LOWER(c.display_name) LIKE $2)
        ORDER BY 2 ASC
        "#,
        q.source,
        pattern,
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => internal_error("history::destinations", e),
    }
}
