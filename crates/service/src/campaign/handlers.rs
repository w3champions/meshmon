//! HTTP handlers for `/api/campaigns/*`.
//!
//! Covers the CRUD + lifecycle surface. The edit-delta, force-pair,
//! pair-list, and preview handlers are added in the next task. Handlers
//! are not yet registered on the axum router — wiring lives in the
//! router-setup task.
//!
//! Every handler mirrors the catalogue surface's error envelope
//! (snake_case `error` key) so the SPA's shared error-handling layer
//! applies without extra branching.

use super::dto::{
    CampaignDto, CampaignListQuery, CreateCampaignRequest, ErrorEnvelope, PatchCampaignRequest,
};
use super::model::PairResolutionState;
use super::repo::{self, CreateInput, RepoError};
use crate::http::auth::AuthSession;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::net::IpAddr;
use std::str::FromStr;
use uuid::Uuid;

/// Render a raw sqlx error as an HTTP 500 with a stable JSON body. Logs
/// the full error server-side so we never leak sqlx-level detail to
/// clients.
fn db_error(context: &'static str, err: sqlx::Error) -> Response {
    tracing::error!(error = %err, "{context}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "database_error" })),
    )
        .into_response()
}

/// Map a domain [`RepoError`] onto the canonical HTTP shape:
/// - `NotFound` → 404 `{"error":"not_found"}`
/// - `IllegalTransition` → 409 `{"error":"illegal_state_transition"}`
/// - `Sqlx` → 500 (via [`db_error`])
fn repo_error(context: &'static str, err: RepoError) -> Response {
    match err {
        RepoError::NotFound(_) => {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
        }
        RepoError::IllegalTransition { .. } => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "illegal_state_transition" })),
        )
            .into_response(),
        RepoError::Sqlx(e) => db_error(context, e),
    }
}

/// `POST /api/campaigns` — create a new campaign in `draft`.
///
/// Rejects blank titles and malformed destination IP strings up front
/// so a client mistake surfaces as 400 rather than leaking a Postgres
/// type error as 500. The `created_by` field is filled from the active
/// session principal; anonymous callers get 401.
#[utoipa::path(
    post,
    path = "/api/campaigns",
    tag = "campaigns",
    request_body = CreateCampaignRequest,
    responses(
        (status = 200, description = "Created campaign", body = CampaignDto),
        (status = 400, description = "Invalid payload", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthSession,
    Json(body): Json<CreateCampaignRequest>,
) -> Response {
    let Some(principal) = auth.user.as_ref() else {
        return (StatusCode::UNAUTHORIZED, "not authenticated").into_response();
    };

    if body.title.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "title_required" })),
        )
            .into_response();
    }

    let destination_ips: Result<Vec<IpAddr>, _> = body
        .destination_ips
        .iter()
        .map(|s| IpAddr::from_str(s))
        .collect();
    let destination_ips = match destination_ips {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_destination_ip" })),
            )
                .into_response();
        }
    };

    let input = CreateInput {
        title: body.title,
        notes: body.notes.unwrap_or_default(),
        protocol: body.protocol,
        source_agent_ids: body.source_agent_ids,
        destination_ips,
        force_measurement: body.force_measurement,
        probe_count: body.probe_count,
        probe_count_detail: body.probe_count_detail,
        timeout_ms: body.timeout_ms,
        probe_stagger_ms: body.probe_stagger_ms,
        loss_threshold_pct: body.loss_threshold_pct,
        stddev_weight: body.stddev_weight,
        evaluation_mode: body.evaluation_mode,
        created_by: Some(principal.username.clone()),
    };

    match repo::create(&state.pool, input).await {
        Ok(row) => (StatusCode::OK, Json(CampaignDto::from(row))).into_response(),
        Err(e) => repo_error("campaign::create", e),
    }
}

/// `GET /api/campaigns` — filtered list of campaigns.
///
/// `limit` is clamped server-side to 500 rows; list responses do not
/// populate `pair_counts` (see [`get_one`] for the per-state counts).
#[utoipa::path(
    get,
    path = "/api/campaigns",
    tag = "campaigns",
    params(CampaignListQuery),
    responses(
        (status = 200, description = "Campaign list", body = Vec<CampaignDto>),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn list(State(state): State<AppState>, Query(q): Query<CampaignListQuery>) -> Response {
    match repo::list(
        &state.pool,
        q.q.as_deref(),
        q.state,
        q.created_by.as_deref(),
        q.limit.min(500),
    )
    .await
    {
        Ok(rows) => (
            StatusCode::OK,
            Json(rows.into_iter().map(CampaignDto::from).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => repo_error("campaign::list", e),
    }
}

/// `GET /api/campaigns/{id}` — single-row fetch + pair-state counts.
///
/// Runs two queries (campaign fetch + `COUNT(*) GROUP BY
/// resolution_state`) and joins the results handler-side. A failure
/// on the counts query degrades gracefully: the campaign body is
/// still returned with an empty `pair_counts` list and the error is
/// logged — the campaign shell is more valuable to the UI than a 500.
#[utoipa::path(
    get,
    path = "/api/campaigns/{id}",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    responses(
        (status = 200, description = "Campaign + pair counts", body = CampaignDto),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign not found", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    let camp = match repo::get(&state.pool, id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response();
        }
        Err(e) => return repo_error("campaign::get", e),
    };

    let counts: Vec<(PairResolutionState, i64)> = match sqlx::query_as::<
        _,
        (PairResolutionState, i64),
    >(
        "SELECT resolution_state, COUNT(*) \
               FROM campaign_pairs \
              WHERE campaign_id = $1 \
              GROUP BY 1",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, campaign_id = %id, "campaign::get_one: pair_counts query failed");
            Vec::new()
        }
    };

    let mut dto = CampaignDto::from(camp);
    dto.pair_counts = counts;
    (StatusCode::OK, Json(dto)).into_response()
}

/// `PATCH /api/campaigns/{id}` — partial update.
///
/// Absent fields leave the underlying column untouched; explicit
/// `null` values are currently treated as "no change" (see
/// [`PatchCampaignRequest`]). Campaign lifecycle state is not
/// editable through this surface — use `/start` / `/stop` / `/edit`.
#[utoipa::path(
    patch,
    path = "/api/campaigns/{id}",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    request_body = PatchCampaignRequest,
    responses(
        (status = 200, description = "Updated campaign", body = CampaignDto),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign not found", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn patch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchCampaignRequest>,
) -> Response {
    match repo::patch(
        &state.pool,
        id,
        body.title.as_deref(),
        body.notes.as_deref(),
        body.loss_threshold_pct,
        body.stddev_weight,
        body.evaluation_mode,
    )
    .await
    {
        Ok(row) => (StatusCode::OK, Json(CampaignDto::from(row))).into_response(),
        Err(e) => repo_error("campaign::patch", e),
    }
}

/// `DELETE /api/campaigns/{id}` — idempotent removal.
///
/// Returns 204 whether or not the row existed (the underlying
/// `DELETE ... WHERE id = $1` is a no-op on an absent id).
#[utoipa::path(
    delete,
    path = "/api/campaigns/{id}",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn delete(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match repo::delete(&state.pool, id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => repo_error("campaign::delete", e),
    }
}

/// `POST /api/campaigns/{id}/start` — transition `draft` → `running`.
///
/// Returns 409 (`illegal_state_transition`) if the campaign is not in
/// `draft`. The scheduler picks up the newly-running campaign via its
/// `LISTEN` loop.
#[utoipa::path(
    post,
    path = "/api/campaigns/{id}/start",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    responses(
        (status = 200, description = "Started", body = CampaignDto),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign not found", body = ErrorEnvelope),
        (status = 409, description = "Illegal state transition", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn start(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match repo::start(&state.pool, id).await {
        Ok(row) => (StatusCode::OK, Json(CampaignDto::from(row))).into_response(),
        Err(e) => repo_error("campaign::start", e),
    }
}

/// `POST /api/campaigns/{id}/stop` — transition `running` → `stopped`.
///
/// Pending pairs are flipped to `skipped` in the same transaction;
/// in-flight `dispatched` pairs settle as-is via the campaign result
/// writer.
#[utoipa::path(
    post,
    path = "/api/campaigns/{id}/stop",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    responses(
        (status = 200, description = "Stopped", body = CampaignDto),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign not found", body = ErrorEnvelope),
        (status = 409, description = "Illegal state transition", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn stop(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match repo::stop(&state.pool, id).await {
        Ok(row) => (StatusCode::OK, Json(CampaignDto::from(row))).into_response(),
        Err(e) => repo_error("campaign::stop", e),
    }
}
