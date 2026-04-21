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

use axum::extract::State;
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
