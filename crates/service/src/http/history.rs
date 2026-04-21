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

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::{types::ipnetwork::IpNetwork, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::campaign::dto::ErrorEnvelope;
use crate::campaign::model::{MeasurementKind, PairResolutionState, ProbeProtocol};
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
