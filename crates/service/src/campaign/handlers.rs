//! HTTP handlers for `/api/campaigns/*`.
//!
//! Covers the full CRUD + lifecycle + edit-delta + pair surface. Routes
//! are registered in `crates/service/src/http/openapi.rs`.
//!
//! Every handler mirrors the catalogue surface's error envelope
//! (snake_case `error` key) so the SPA's shared error-handling layer
//! applies without extra branching.

use super::broker::CampaignStreamEvent;
use super::dto::{
    CampaignDto, CampaignListQuery, CreateCampaignRequest, DetailRequest, DetailResponse,
    DetailScope, EditCampaignRequest, EditPairDto, ErrorEnvelope, EvaluationDto,
    EvaluationResultsDto, ForcePairRequest, PairDto, PairListQuery, PatchCampaignRequest,
    PreviewDispatchResponse,
};
use super::eval::{self, EvalError};
use super::model::{CampaignState, PairResolutionState};
use super::repo::{self, CreateInput, EditInput, EvaluationRow, RepoError};
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
    operation_id = "campaigns_list",
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
    operation_id = "campaigns_get_one",
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

    // Baseline-only: `pair_counts` tracks the campaign's own dispatch
    // lifecycle for operators. `/detail`-triggered rows have their own
    // state and must not inflate counters like `pending` when the
    // campaign itself is otherwise settled.
    let counts: Vec<(PairResolutionState, i64)> = match sqlx::query_as::<
        _,
        (PairResolutionState, i64),
    >(
        "SELECT resolution_state, COUNT(*) \
               FROM campaign_pairs \
              WHERE campaign_id = $1 \
                AND kind = 'campaign' \
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
    operation_id = "campaigns_patch",
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
    // Mirror the `create` invariant: if a title is supplied, it must
    // be non-blank. `None` leaves the stored title untouched.
    if let Some(t) = body.title.as_ref() {
        if t.trim().is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "title_required" })),
            )
                .into_response();
        }
    }
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
    operation_id = "campaigns_delete",
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

/// Parse a list of [`EditPairDto`]s into typed `(source, IpAddr)` tuples.
///
/// Returns a small marker error on the first malformed `destination_ip`
/// so the caller can shape a single 400 response without carrying a
/// full `Response` across the `Result` boundary (which clippy flags as
/// `result_large_err`).
struct InvalidDestinationIp;

fn parse_pairs(pairs: &[EditPairDto]) -> Result<Vec<(String, IpAddr)>, InvalidDestinationIp> {
    pairs
        .iter()
        .map(|p| {
            IpAddr::from_str(&p.destination_ip)
                .map(|ip| (p.source_agent_id.clone(), ip))
                .map_err(|_| InvalidDestinationIp)
        })
        .collect()
}

/// Build the canonical 400 body for a rejected destination IP.
fn invalid_destination_ip_response() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "invalid_destination_ip" })),
    )
        .into_response()
}

/// `POST /api/campaigns/{id}/edit` — apply an edit delta.
///
/// Adds/removes pairs on a finished campaign (`completed`, `stopped`,
/// or `evaluated`) and transitions it back to `running`. When
/// `force_measurement` is `Some(true)`, the sticky flag is flipped and
/// every non-delta pair is reset so the whole campaign re-runs.
#[utoipa::path(
    post,
    path = "/api/campaigns/{id}/edit",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    request_body = EditCampaignRequest,
    responses(
        (status = 200, description = "Updated campaign", body = CampaignDto),
        (status = 400, description = "Invalid payload", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign not found", body = ErrorEnvelope),
        (status = 409, description = "Illegal state transition", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn edit(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<EditCampaignRequest>,
) -> Response {
    let add_pairs = match parse_pairs(&body.add_pairs) {
        Ok(v) => v,
        Err(_) => return invalid_destination_ip_response(),
    };
    let remove_pairs = match parse_pairs(&body.remove_pairs) {
        Ok(v) => v,
        Err(_) => return invalid_destination_ip_response(),
    };
    let input = EditInput {
        add_pairs,
        remove_pairs,
        force_measurement: body.force_measurement,
    };
    match repo::apply_edit(&state.pool, id, input).await {
        Ok(row) => (StatusCode::OK, Json(CampaignDto::from(row))).into_response(),
        Err(e) => repo_error("campaign::edit", e),
    }
}

/// `POST /api/campaigns/{id}/force_pair` — reset a single pair and
/// re-enter `running`.
///
/// 404 when the `(source_agent_id, destination_ip)` pair is unknown for
/// the campaign; 400 if the destination IP fails to parse.
#[utoipa::path(
    post,
    path = "/api/campaigns/{id}/force_pair",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    request_body = ForcePairRequest,
    responses(
        (status = 200, description = "Pair reset + campaign running", body = CampaignDto),
        (status = 400, description = "Invalid payload", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 404, description = "Pair not found", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn force_pair(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ForcePairRequest>,
) -> Response {
    let ip = match IpAddr::from_str(&body.destination_ip) {
        Ok(ip) => ip,
        Err(_) => return invalid_destination_ip_response(),
    };
    match repo::force_pair(&state.pool, id, &body.source_agent_id, ip).await {
        Ok(row) => (StatusCode::OK, Json(CampaignDto::from(row))).into_response(),
        Err(e) => repo_error("campaign::force_pair", e),
    }
}

/// `GET /api/campaigns/{id}/pairs` — paginated pair list.
///
/// Empty `state` filter expands to all six pair-resolution states.
/// `limit` is clamped to 5 000 rows handler-side; the repo clamps
/// further to its own upper bound.
#[utoipa::path(
    get,
    path = "/api/campaigns/{id}/pairs",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id"), PairListQuery),
    responses(
        (status = 200, description = "Pair list", body = Vec<PairDto>),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn pairs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<PairListQuery>,
) -> Response {
    let states = if q.state.is_empty() {
        vec![
            PairResolutionState::Pending,
            PairResolutionState::Dispatched,
            PairResolutionState::Reused,
            PairResolutionState::Succeeded,
            PairResolutionState::Unreachable,
            PairResolutionState::Skipped,
        ]
    } else {
        q.state
    };
    match repo::list_pairs(&state.pool, id, &states, q.limit.min(5000)).await {
        Ok(rows) => (
            StatusCode::OK,
            Json(rows.into_iter().map(PairDto::from).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => repo_error("campaign::pairs", e),
    }
}

/// `GET /api/campaigns/{id}/preview-dispatch-count` — dispatch estimate.
///
/// Counts the campaign's actual `campaign_pairs` rows, splitting them
/// between ones resolvable from the 24 h reuse window and ones the
/// scheduler would dispatch fresh. Never writes.
#[utoipa::path(
    get,
    path = "/api/campaigns/{id}/preview-dispatch-count",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    responses(
        (status = 200, description = "Dispatch preview", body = PreviewDispatchResponse),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign not found", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn preview_dispatch_count(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(camp) = (match repo::get(&state.pool, id).await {
        Ok(v) => v,
        Err(e) => return repo_error("campaign::preview", e),
    }) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response();
    };

    match repo::preview_dispatch_count_for_campaign(
        &state.pool,
        id,
        camp.protocol,
        camp.force_measurement,
    )
    .await
    {
        Ok(counts) => (
            StatusCode::OK,
            Json(PreviewDispatchResponse {
                total: counts.total,
                reusable: counts.reusable,
                fresh: counts.fresh,
            }),
        )
            .into_response(),
        Err(e) => repo_error("campaign::preview_dispatch_count", e),
    }
}

/// Convert an [`EvaluationRow`] into the wire DTO.
///
/// The stored `results` column is owned by this service (written by
/// [`repo::write_evaluation`] as a serialised [`EvaluationResultsDto`]),
/// so a deserialisation failure here signals data corruption from outside
/// the normal write path. Returns `Err` so the caller can map it to a
/// `500 invalid_evaluation_payload` response instead of panicking a tokio
/// worker.
fn to_evaluation_dto(row: EvaluationRow) -> Result<EvaluationDto, serde_json::Error> {
    let results: EvaluationResultsDto = serde_json::from_value(row.results)?;
    Ok(EvaluationDto {
        campaign_id: row.campaign_id,
        evaluated_at: row.evaluated_at,
        loss_threshold_pct: row.loss_threshold_pct,
        stddev_weight: row.stddev_weight,
        evaluation_mode: row.evaluation_mode,
        baseline_pair_count: row.baseline_pair_count,
        candidates_total: row.candidates_total,
        candidates_good: row.candidates_good,
        avg_improvement_ms: row.avg_improvement_ms,
        results,
    })
}

fn invalid_evaluation_payload(campaign_id: Uuid, err: serde_json::Error) -> Response {
    tracing::error!(
        %campaign_id,
        error = %err,
        "campaign_evaluations.results failed to deserialise"
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "invalid_evaluation_payload" })),
    )
        .into_response()
}

/// `POST /api/campaigns/{id}/evaluate` — run the evaluator and persist.
///
/// Gated on `state IN ('completed','evaluated')`: re-running on an
/// already-evaluated campaign is allowed so operators can retune
/// `loss_threshold_pct` / `stddev_weight` without editing the row. When
/// the campaign was in `completed`, a best-effort transition flips it to
/// `evaluated`; a concurrent transition that loses the gate still leaves
/// the evaluation row written and the SSE event fired.
///
/// Returns 422 (`no_baseline_pairs`) when the evaluator finds no
/// agent→agent baseline — the campaign must include at least one pair
/// whose destination IP belongs to a registered agent.
#[utoipa::path(
    post,
    path = "/api/campaigns/{id}/evaluate",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    responses(
        (status = 200, description = "Evaluation written", body = EvaluationDto),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign not found", body = ErrorEnvelope),
        (status = 409, description = "Campaign not in completed/evaluated state", body = ErrorEnvelope),
        (status = 422, description = "No baseline (agent→agent) pairs", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn evaluate(
    State(state): State<AppState>,
    _auth: AuthSession,
    Path(id): Path<Uuid>,
) -> Response {
    let campaign = match repo::get(&state.pool, id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response();
        }
        Err(e) => return repo_error("campaign::evaluate", e),
    };
    if !matches!(
        campaign.state,
        CampaignState::Completed | CampaignState::Evaluated
    ) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "illegal_state_transition" })),
        )
            .into_response();
    }

    let inputs = match repo::measurements_for_campaign(&state.pool, id).await {
        Ok(i) => i,
        Err(e) => return repo_error("campaign::evaluate::inputs", e),
    };
    // Snapshot the evaluator knobs from `inputs` (not `campaign`) so a
    // concurrent `PATCH /campaigns/{id}` that lands between the two reads
    // cannot desync the scored knobs from the persisted evaluation row.
    // `campaign` stays in use only for its `state` gate above; it is no
    // longer the source of knob values.
    let loss_threshold_pct = inputs.loss_threshold_pct;
    let stddev_weight = inputs.stddev_weight;
    let evaluation_mode = inputs.mode;
    let outputs = match eval::evaluate(inputs) {
        Ok(o) => o,
        Err(EvalError::NoBaseline) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "no_baseline_pairs" })),
            )
                .into_response();
        }
    };

    // `write_evaluation` writes the evaluation row AND promotes
    // `completed → evaluated` in the same transaction. A crash between
    // the UPSERT and the state flip would otherwise leave the campaign
    // stuck in `completed` with a written evaluation row (inconsistent).
    let row = match repo::write_evaluation(
        &state.pool,
        id,
        &outputs,
        loss_threshold_pct,
        stddev_weight,
        evaluation_mode,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return repo_error("campaign::evaluate::write", e),
    };

    // No in-process broker publish here. The `campaign_evaluations_notify`
    // trigger fires `campaign_evaluated` inside the same transaction, and
    // the campaign SSE listener fans that out to every subscriber (this
    // instance's and its peers') — a direct publish here would cause
    // same-instance clients to receive a duplicate `evaluated` frame.

    match to_evaluation_dto(row) {
        Ok(dto) => (StatusCode::OK, Json(dto)).into_response(),
        Err(e) => invalid_evaluation_payload(id, e),
    }
}

/// `GET /api/campaigns/{id}/evaluation` — read-through on the persisted
/// evaluation row.
///
/// 404 (`not_evaluated`) when the campaign has never been evaluated.
/// The same snake_case envelope pattern as every other handler lets the
/// SPA's shared error layer branch on the stable code without parsing
/// prose.
#[utoipa::path(
    get,
    path = "/api/campaigns/{id}/evaluation",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    responses(
        (status = 200, description = "Evaluation result", body = EvaluationDto),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign not evaluated", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn get_evaluation(
    State(state): State<AppState>,
    _auth: AuthSession,
    Path(id): Path<Uuid>,
) -> Response {
    match repo::read_evaluation(&state.pool, id).await {
        Ok(Some(row)) => match to_evaluation_dto(row) {
            Ok(dto) => (StatusCode::OK, Json(dto)).into_response(),
            Err(e) => invalid_evaluation_payload(id, e),
        },
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "not_evaluated" })),
        )
            .into_response(),
        Err(e) => repo_error("campaign::get_evaluation", e),
    }
}

/// `POST /api/campaigns/{id}/detail` — enqueue detail-ping + detail-mtr
/// rows for a slice of `(source_agent, destination_ip)` pairs.
///
/// Gated on `state IN ('completed','evaluated')`. The three scopes:
///
/// - `all`: every `kind='campaign'` pair whose baseline resolution
///   `succeeded` or `reused`. Selects directly from `campaign_pairs`
///   because an evaluation row is not required for this scope.
/// - `good_candidates`: pulls the persisted evaluation row and expands
///   each qualifying `(source_agent, destination_agent, transit_ip)`
///   triple into a pair of `(source_agent → transit_ip)` +
///   `(destination_agent → transit_ip)` detail entries so both legs
///   get a higher-resolution measurement. Requires a prior evaluation;
///   400 with `no_evaluation` otherwise.
/// - `pair`: a single operator-chosen `(source_agent_id,
///   destination_ip)` tuple from the request body.
///
/// All scopes flow through [`repo::insert_detail_pairs`], which owns
/// the transition back to `running` and emits `campaign_state_changed`
/// through the Postgres NOTIFY trigger. The handler additionally
/// publishes `CampaignStreamEvent::StateChanged { running }` for the
/// same campaign so clients subscribed to the broker see the
/// transition even before the NOTIFY listener wakes up — but **only**
/// when a real transition occurred (`state_changed=true`). A
/// no-op call against an already-`running` campaign skips the
/// broadcast to avoid spurious client refreshes.
#[utoipa::path(
    post,
    path = "/api/campaigns/{id}/detail",
    tag = "campaigns",
    params(("id" = Uuid, Path, description = "Campaign id")),
    request_body = DetailRequest,
    responses(
        (status = 200, description = "Pairs enqueued", body = DetailResponse),
        (status = 400, description = "Bad scope payload or no_evaluation precondition", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign not found", body = ErrorEnvelope),
        (status = 409, description = "Campaign not in completed/evaluated state", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn detail(
    State(state): State<AppState>,
    _auth: AuthSession,
    Path(id): Path<Uuid>,
    Json(body): Json<DetailRequest>,
) -> Response {
    let campaign = match repo::get(&state.pool, id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response();
        }
        Err(e) => return repo_error("campaign::detail", e),
    };
    if !matches!(
        campaign.state,
        CampaignState::Completed | CampaignState::Evaluated
    ) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "illegal_state_transition" })),
        )
            .into_response();
    }

    let pairs: Vec<(String, IpAddr)> = match body.scope {
        DetailScope::All => match repo::settled_campaign_pairs(&state.pool, id).await {
            Ok(p) => p,
            Err(e) => return repo_error("campaign::detail::all", e),
        },
        DetailScope::GoodCandidates => {
            let row = match repo::read_evaluation(&state.pool, id).await {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": "no_evaluation" })),
                    )
                        .into_response();
                }
                Err(e) => return repo_error("campaign::detail::eval", e),
            };
            let results: EvaluationResultsDto = match serde_json::from_value(row.results) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        campaign_id = %id,
                        "campaign::detail: stored evaluation row failed to deserialise"
                    );
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": "invalid_evaluation_payload" })),
                    )
                        .into_response();
                }
            };
            let mut acc: Vec<(String, IpAddr)> = Vec::new();
            for cand in results.candidates.iter().filter(|c| c.pairs_improved >= 1) {
                let Ok(transit_ip) = IpAddr::from_str(&cand.destination_ip) else {
                    tracing::warn!(
                        campaign_id = %id,
                        destination_ip = %cand.destination_ip,
                        "campaign::detail: skipping candidate with unparseable destination_ip"
                    );
                    continue;
                };
                for pd in cand.pair_details.iter().filter(|p| p.qualifies) {
                    acc.push((pd.source_agent_id.clone(), transit_ip));
                    acc.push((pd.destination_agent_id.clone(), transit_ip));
                }
            }
            acc.sort();
            acc.dedup();
            acc
        }
        DetailScope::Pair => {
            let Some(p) = body.pair else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "missing_pair" })),
                )
                    .into_response();
            };
            let Ok(ip) = IpAddr::from_str(&p.destination_ip) else {
                return invalid_destination_ip_response();
            };
            vec![(p.source_agent_id, ip)]
        }
    };

    if pairs.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no_pairs_selected" })),
        )
            .into_response();
    }

    let (enqueued, state_changed) = match repo::insert_detail_pairs(&state.pool, id, &pairs).await {
        Ok(out) => out,
        Err(e) => return repo_error("campaign::detail::insert", e),
    };

    // Only publish the SSE state-change when an actual transition
    // occurred. A campaign already in `running` (e.g. operator racing
    // with the scheduler) keeps its prior state and does not emit a
    // synthetic `StateChanged { running }` event.
    if state_changed {
        state
            .campaign_broker
            .publish(CampaignStreamEvent::StateChanged {
                campaign_id: id,
                state: CampaignState::Running,
            });
    }

    // Post-call state: the canonical transition promoted
    // Completed|Evaluated to Running when `state_changed`; otherwise the
    // campaign stayed in its prior state (which, per the state gate at
    // the top of this handler, is one of {Completed, Evaluated} —
    // except for the race where a concurrent writer already flipped it
    // to Running, in which case the prior read is stale). Report the
    // prior-read state in the stable-case, which is accurate for every
    // caller that was not racing another writer.
    let campaign_state = if state_changed {
        CampaignState::Running
    } else {
        campaign.state
    };

    (
        StatusCode::OK,
        Json(DetailResponse {
            pairs_enqueued: enqueued,
            campaign_state,
        }),
    )
        .into_response()
}
