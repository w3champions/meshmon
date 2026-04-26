//! HTTP handlers for `/api/campaigns/*`.
//!
//! Covers the full CRUD + lifecycle + edit-delta + pair surface. Routes
//! are registered in `crates/service/src/http/openapi.rs`.
//!
//! Every handler mirrors the catalogue surface's error envelope
//! (snake_case `error` key) so the SPA's shared error-handling layer
//! applies without extra branching.

use super::cursor::{CursorError, PairDetailCursor};
use super::dto::{
    CampaignDto, CampaignListQuery, CreateCampaignRequest, DetailRequest, DetailResponse,
    DetailScope, EdgePairsListResponse, EdgePairsQuery, EditCampaignRequest, EditPairDto,
    ErrorEnvelope, EvaluationDto, EvaluationPairDetailListResponse, EvaluationPairDetailQuery,
    ForcePairRequest, PairDto, PairListQuery, PatchCampaignRequest, PreviewDispatchResponse,
};
use super::eval::{self, AttributedMeasurement, EvalError};
use super::evaluation_repo::{self, EdgePairLookup, PairDetailLookup};
use super::model::{
    CampaignState, DirectSource, EvaluationMode, PairResolutionState, ProbeProtocol,
};
use super::repo::{self, CreateInput, EditInput, RepoError};
use crate::hostname::session_id_from_auth;
use crate::hostname::stamp::bulk_hostnames_and_enqueue;
use crate::http::auth::AuthSession;
use crate::state::AppState;
use crate::vm_query::{self, VmQueryError};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;

/// Map a [`ProbeProtocol`] to the `protocol=` label value used by the
/// ingestion pipeline's metrics emitter (see
/// `ingestion::protocol_label`). Keeping the mapping local so the
/// evaluator's VM read path does not take a dependency on the internal
/// `meshmon_protocol::Protocol` wire enum.
fn protocol_label(p: ProbeProtocol) -> &'static str {
    match p {
        ProbeProtocol::Icmp => "icmp",
        ProbeProtocol::Tcp => "tcp",
        ProbeProtocol::Udp => "udp",
    }
}

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

/// Validate the edge-candidate-mode knobs: `useful_latency_ms`,
/// `max_hops`, and `vm_lookback_minutes`.
///
/// Rules:
/// - `edge_candidate` mode requires `useful_latency_ms` to be `Some` and
///   positive. For other modes a supplied value must still be positive.
/// - `max_hops` must be in `[0, 2]`. A value of `0` is only valid for
///   `edge_candidate` mode.
/// - `vm_lookback_minutes` must be in `[1, 1440]`.
fn validate_create_or_patch_knobs(
    mode: super::model::EvaluationMode,
    useful_latency_ms: Option<f32>,
    max_hops: Option<i16>,
    vm_lookback_minutes: Option<i32>,
) -> Result<(), (StatusCode, Json<ErrorEnvelope>)> {
    use super::model::EvaluationMode;

    // useful_latency_ms: required + positive for edge_candidate; positive
    // if supplied for any other mode.
    if mode == EvaluationMode::EdgeCandidate {
        match useful_latency_ms {
            None => return Err(error_400("useful_latency_required")),
            Some(v) if v.is_nan() || v <= 0.0 => return Err(error_400("useful_latency_invalid")),
            _ => {}
        }
    } else if let Some(v) = useful_latency_ms {
        if v.is_nan() || v <= 0.0 {
            return Err(error_400("useful_latency_invalid"));
        }
    }

    // max_hops: must be in [0, 2]; 0 only allowed for edge_candidate.
    if let Some(h) = max_hops {
        if !(0..=2).contains(&h) {
            return Err(error_400("max_hops_out_of_range"));
        }
        if h == 0 && mode != EvaluationMode::EdgeCandidate {
            return Err(error_400("max_hops_invalid_for_mode"));
        }
    }

    // vm_lookback_minutes: must be in [1, 1440].
    if let Some(m) = vm_lookback_minutes {
        if !(1..=1440).contains(&m) {
            return Err(error_400("vm_lookback_out_of_range"));
        }
    }

    Ok(())
}

/// Build a 400 Bad Request JSON response with a stable `error` code.
fn error_400(code: &'static str) -> (StatusCode, Json<ErrorEnvelope>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorEnvelope { error: code.into() }),
    )
}

/// Build a 422 Unprocessable Entity JSON response with a stable `error` code.
fn error_422(code: &'static str) -> (StatusCode, Json<ErrorEnvelope>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ErrorEnvelope { error: code.into() }),
    )
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

    // Resolve the effective mode (default: Optimization) and validate the
    // edge-candidate knobs before touching the database.
    let mode = body
        .evaluation_mode
        .unwrap_or(super::model::EvaluationMode::Optimization);
    if let Err(e) = validate_create_or_patch_knobs(
        mode,
        body.useful_latency_ms,
        body.max_hops,
        body.vm_lookback_minutes,
    ) {
        return e.into_response();
    }

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
        loss_threshold_ratio: body.loss_threshold_ratio,
        stddev_weight: body.stddev_weight,
        evaluation_mode: body.evaluation_mode,
        max_transit_rtt_ms: body.max_transit_rtt_ms,
        max_transit_stddev_ms: body.max_transit_stddev_ms,
        min_improvement_ms: body.min_improvement_ms,
        min_improvement_ratio: body.min_improvement_ratio,
        useful_latency_ms: body.useful_latency_ms,
        max_hops: body.max_hops,
        vm_lookback_minutes: body.vm_lookback_minutes,
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

    let source_agent_ids = source_agent_ids_for_campaign(&state.pool, id).await;

    let mut dto = CampaignDto::from(camp);
    dto.pair_counts = counts;
    dto.source_agent_ids = source_agent_ids;
    (StatusCode::OK, Json(dto)).into_response()
}

/// Distinct source-agent ids on a campaign's `campaign_pairs`, ascending.
/// Used to surface the picker contents on single-row campaign reads
/// (GET / PATCH) so the SPA can render the source-agent selector without
/// a second round-trip. A query failure degrades to an empty list — the
/// campaign shell is still useful and the failure is logged.
async fn source_agent_ids_for_campaign(pool: &sqlx::PgPool, id: Uuid) -> Vec<String> {
    match sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT source_agent_id \
           FROM campaign_pairs \
          WHERE campaign_id = $1 \
            AND kind = 'campaign' \
          ORDER BY source_agent_id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                error = %e,
                campaign_id = %id,
                "campaign::source_agent_ids_for_campaign query failed",
            );
            Vec::new()
        }
    }
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

    // Fetch the existing row to resolve the effective mode for PATCH
    // (since evaluation_mode is optional and defaults to the stored value)
    // and to compare knob values for the dismissal check below.
    let existing = match repo::get(&state.pool, id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response();
        }
        Err(e) => return repo_error("campaign::patch::prefetch", e),
    };

    // Resolve the effective row-after-PATCH state. PATCH columns are
    // applied via `COALESCE($n, col)`, so any field absent from the body
    // keeps its stored value. Validation runs against the post-PATCH
    // shape — otherwise a metadata-only PATCH against an
    // `edge_candidate` row would trip `useful_latency_required` even
    // though `useful_latency_ms` would be untouched.
    let mode = body.evaluation_mode.unwrap_or(existing.evaluation_mode);
    let effective_useful_latency_ms = body.useful_latency_ms.or(existing.useful_latency_ms);
    let effective_max_hops = body.max_hops.unwrap_or(existing.max_hops);
    let effective_vm_lookback = body
        .vm_lookback_minutes
        .unwrap_or(existing.vm_lookback_minutes);

    if let Err(e) = validate_create_or_patch_knobs(
        mode,
        effective_useful_latency_ms,
        Some(effective_max_hops),
        Some(effective_vm_lookback),
    ) {
        return e.into_response();
    }

    // Determine whether any evaluator knob has actually changed. When a
    // knob is absent from the PATCH body, COALESCE leaves it untouched in
    // the DB — so only supplied values can trigger a dismissal. Each
    // clause compares the body's optional value against the stored
    // representation; the `Option<T>`-vs-`T` mismatch in a few fields
    // is intentional and matches the column nullability.
    let knob_changed = body
        .useful_latency_ms
        .is_some_and(|v| Some(v) != existing.useful_latency_ms)
        || body.max_hops.is_some_and(|v| v != existing.max_hops)
        || body
            .vm_lookback_minutes
            .is_some_and(|v| v != existing.vm_lookback_minutes)
        || body
            .loss_threshold_ratio
            .is_some_and(|v| v != existing.loss_threshold_ratio)
        || body
            .stddev_weight
            .is_some_and(|v| v != existing.stddev_weight)
        || body
            .evaluation_mode
            .is_some_and(|v| v != existing.evaluation_mode)
        || body
            .max_transit_rtt_ms
            .is_some_and(|v| Some(v) != existing.max_transit_rtt_ms)
        || body
            .max_transit_stddev_ms
            .is_some_and(|v| Some(v) != existing.max_transit_stddev_ms)
        || body
            .min_improvement_ms
            .is_some_and(|v| Some(v) != existing.min_improvement_ms)
        || body
            .min_improvement_ratio
            .is_some_and(|v| Some(v) != existing.min_improvement_ratio);

    let updated = match repo::patch(
        &state.pool,
        id,
        body.title.as_deref(),
        body.notes.as_deref(),
        body.loss_threshold_ratio,
        body.stddev_weight,
        body.evaluation_mode,
        body.max_transit_rtt_ms,
        body.max_transit_stddev_ms,
        body.min_improvement_ms,
        body.min_improvement_ratio,
        body.useful_latency_ms,
        body.max_hops,
        body.vm_lookback_minutes,
    )
    .await
    {
        Ok(row) => row,
        Err(e) => return repo_error("campaign::patch", e),
    };

    // Dismiss the existing evaluation when a knob that affects the scoring
    // outcome has changed. This keeps the persisted evaluation consistent
    // with the current knob values and signals to the operator that a
    // re-evaluate is needed. After dismissal, re-fetch the row so the
    // PATCH response body reflects the post-dismiss state (state back to
    // `completed`, `evaluated_at` cleared) instead of the pre-dismiss
    // snapshot returned by the UPDATE.
    let response_row = if knob_changed {
        if let Err(e) = evaluation_repo::dismiss_evaluation(&state.pool, id).await {
            return db_error("campaign::patch::dismiss_evaluation", e);
        }
        match repo::get(&state.pool, id).await {
            Ok(Some(row)) => row,
            Ok(None) => return repo_error("campaign::patch::refetch", RepoError::NotFound(id)),
            Err(e) => return repo_error("campaign::patch::refetch", e),
        }
    } else {
        updated
    };

    // Surface the source-agent picker contents on PATCH responses for
    // parity with `get_one`, so SPA cache updates don't drop the field.
    let source_agent_ids = source_agent_ids_for_campaign(&state.pool, id).await;
    let mut dto = CampaignDto::from(response_row);
    dto.source_agent_ids = source_agent_ids;
    (StatusCode::OK, Json(dto)).into_response()
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

/// Stamp `destination_hostname` on a slice of [`PairDto`]s.
///
/// Collects all destination IPs, bulk-resolves in one DB round-trip,
/// and writes matched hostnames back. Non-fatal: on DB error, logs at
/// `warn!` and returns without hostnames.
async fn stamp_pair_dtos(
    state: &AppState,
    session: &crate::hostname::SessionId,
    pairs: &mut [PairDto],
) {
    let ips: Vec<IpAddr> = pairs
        .iter()
        .filter_map(|p| p.destination_ip.parse::<IpAddr>().ok())
        .collect();
    if ips.is_empty() {
        return;
    }
    match bulk_hostnames_and_enqueue(state, session, &ips).await {
        Ok(map) => {
            for pair in pairs.iter_mut() {
                if let Ok(ip) = pair.destination_ip.parse::<IpAddr>() {
                    if let Some(Some(h)) = map.get(&ip) {
                        pair.destination_hostname = Some(h.clone());
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "campaign::pairs: hostname stamp failed; returning unhostnamed response");
        }
    }
}

/// Stamp `hostname` on [`EvaluationDto`] candidates.
///
/// Collects every candidate's `destination_ip`, bulk-resolves in one DB
/// round-trip, and writes matched hostnames back. Pair-detail rows are
/// no longer carried on the wire DTO — the paginated
/// `…/candidates/{ip}/pair_details` endpoint stamps its own page-level
/// hostnames in `get_candidate_pair_details`.
///
/// Non-fatal: on DB error, logs at `warn!` and returns without
/// hostnames.
async fn stamp_evaluation_dto(
    state: &AppState,
    session: &crate::hostname::SessionId,
    dto: &mut EvaluationDto,
) {
    let ips: Vec<IpAddr> = dto
        .results
        .candidates
        .iter()
        .filter_map(|c| c.destination_ip.parse::<IpAddr>().ok())
        .collect();

    if ips.is_empty() {
        return;
    }

    match bulk_hostnames_and_enqueue(state, session, &ips).await {
        Ok(map) => {
            for cand in dto.results.candidates.iter_mut() {
                if let Ok(ip) = cand.destination_ip.parse::<IpAddr>() {
                    if let Some(Some(h)) = map.get(&ip) {
                        cand.hostname = Some(h.clone());
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "campaign::evaluation: hostname stamp failed; returning unhostnamed response");
        }
    }
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
    auth: AuthSession,
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
    let mut dtos = match repo::list_pairs(&state.pool, id, &states, q.limit.min(5000)).await {
        Ok(rows) => rows.into_iter().map(PairDto::from).collect::<Vec<_>>(),
        Err(e) => return repo_error("campaign::pairs", e),
    };
    let session = session_id_from_auth(&auth);
    stamp_pair_dtos(&state, &session, &mut dtos).await;
    (StatusCode::OK, Json(dtos)).into_response()
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

/// Fetch VictoriaMetrics continuous-mesh baselines for agent→agent pairs
/// the active-probe data didn't cover and synthesize
/// [`AttributedMeasurement`] rows carrying
/// [`DirectSource::VmContinuous`] for use by the evaluator.
///
/// Returns an empty vector when:
/// * `vm_url_opt` is `None` (silent degrade; operator sees 422 if
///   active-probe data is also absent),
/// * the roster is empty,
/// * no agent→agent pair is missing from the active-probe set,
/// * VM returned nothing usable for any missing pair.
///
/// Errors are propagated verbatim from [`vm_query::fetch_agent_baselines`]
/// so the caller can map `NotConfigured` to silent skip and the remaining
/// variants to a single 503 response.
async fn fetch_and_synthesize_vm_baselines(
    vm_url_opt: Option<&str>,
    protocol: ProbeProtocol,
    active: &[AttributedMeasurement],
    roster: &[eval::AgentRow],
    vm_lookback_minutes: i32,
) -> Result<Vec<AttributedMeasurement>, VmQueryError> {
    let Some(vm_url) = vm_url_opt else {
        return Ok(Vec::new());
    };
    if roster.is_empty() {
        return Ok(Vec::new());
    }

    // `covered` only counts rows with a latency value; a rtt-less row
    // (e.g. 100 %-loss success) doesn't stop us from asking VM for a
    // usable baseline on the same pair.
    let covered: HashSet<(String, IpAddr)> = active
        .iter()
        .filter(|m| m.latency_avg_ms.is_some())
        .map(|m| (m.source_agent_id.clone(), m.destination_ip))
        .collect();

    let missing: HashSet<(String, IpAddr)> = roster
        .iter()
        .flat_map(|a| {
            roster
                .iter()
                .filter(move |b| a.agent_id != b.agent_id)
                .map(move |b| (a.agent_id.clone(), b.ip))
        })
        .filter(|pair| !covered.contains(pair))
        .collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }

    let roster_ids: Vec<String> = roster.iter().map(|a| a.agent_id.clone()).collect();
    // Convert the campaign's `vm_lookback_minutes` knob to a `Duration` for the
    // PromQL `avg_over_time([Xm])` window. Clamp to 1 minute on the low end
    // (schema validates [1, 1440] so this is defence-in-depth).
    let lookback = Duration::from_secs(vm_lookback_minutes.max(1) as u64 * 60);
    let samples =
        vm_query::fetch_agent_baselines(vm_url, &roster_ids, protocol_label(protocol), lookback)
            .await?;

    let ip_by_id: HashMap<&str, IpAddr> =
        roster.iter().map(|a| (a.agent_id.as_str(), a.ip)).collect();

    let mut synthesized: Vec<AttributedMeasurement> = Vec::with_capacity(samples.len());
    for sample in &samples {
        let Some(&target_ip) = ip_by_id.get(sample.target_agent_id.as_str()) else {
            // VM surfaced a label we don't recognise as a roster
            // agent (IP rebind, stale label cache, etc.). Ignore it —
            // the evaluator only needs pairs in the current roster.
            continue;
        };
        let Some(&source_ip) = ip_by_id.get(sample.source_agent_id.as_str()) else {
            continue;
        };
        if source_ip == target_ip {
            continue;
        }
        if !missing.contains(&(sample.source_agent_id.clone(), target_ip)) {
            // Active-probe data already covers this pair; skip.
            continue;
        }
        let Some(latency_avg_ms) = sample.latency_avg_ms else {
            // No RTT → unusable downstream; the evaluator would short-
            // circuit on `Some(direct_rtt)` anyway.
            continue;
        };
        synthesized.push(AttributedMeasurement {
            source_agent_id: sample.source_agent_id.clone(),
            destination_ip: target_ip,
            latency_avg_ms: Some(latency_avg_ms),
            latency_stddev_ms: sample.latency_stddev_ms,
            loss_ratio: sample.loss_ratio.unwrap_or(0.0),
            mtr_measurement_id: None,
            direct_source: DirectSource::VmContinuous,
        });
    }

    Ok(synthesized)
}

/// `POST /api/campaigns/{id}/evaluate` — run the evaluator and persist.
///
/// Gated on `state IN ('completed','evaluated')`: re-running on an
/// already-evaluated campaign is allowed so operators can retune
/// `loss_threshold_ratio` / `stddev_weight` without editing the row. When
/// the campaign was in `completed`, a best-effort transition flips it to
/// `evaluated`; a concurrent transition that loses the gate still leaves
/// the evaluation row written and the SSE event fired.
///
/// Before the evaluator runs, the handler augments the active-probe
/// baseline set with VictoriaMetrics continuous-mesh samples for every
/// agent→agent pair the campaign's `measurements` rows didn't already
/// cover. VM-sourced rows carry `direct_source='vm_continuous'` on the
/// resulting `campaign_evaluation_pair_details`. When `upstream.vm_url`
/// isn't configured the handler silently falls back to active-probe
/// data only.
///
/// For `edge_candidate` campaigns the handler also folds in the reverse-
/// direction measurements (B→X and X→A legs) collected by
/// [`repo::reverse_direction_measurements_for_campaign`] before VM
/// synthesis. The reverse set applies cross-mode — the symmetry-fallback
/// path in [`crate::campaign::eval::legs::LegLookup`] uses these rows
/// for both EdgeCandidate and Triple modes.
///
/// Returns:
/// * 422 (`no_destinations`) — EdgeCandidate mode with no destination IPs
///   configured (the campaign has no `destination_ips`).
/// * 422 (`no_candidates_with_data`) — EdgeCandidate mode with no usable
///   measurements after VM synthesis.
/// * 422 (`no_baseline_pairs`) — Triple mode with no agent→agent baseline
///   available, even after the VM fallback (or VM wasn't configured).
/// * 503 (`vm_upstream`) — VM was configured but the query failed
///   (unreachable, non-2xx, malformed response).
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
        (status = 422, description = "no_destinations | no_candidates_with_data | no_baseline_pairs", body = ErrorEnvelope),
        (status = 503, description = "VictoriaMetrics upstream unreachable", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn evaluate(
    State(state): State<AppState>,
    auth: AuthSession,
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

    let (mut inputs, vm_lookback_minutes) =
        match repo::measurements_for_campaign(&state.pool, id).await {
            Ok(pair) => pair,
            Err(e) => return repo_error("campaign::evaluate::inputs", e),
        };
    // Snapshot the evaluator knobs from `inputs` (not `campaign`) so a
    // concurrent `PATCH /campaigns/{id}` that lands between the two reads
    // cannot desync the scored knobs from the persisted evaluation row.
    // `campaign` stays in use only for its `state` gate above; it is no
    // longer the source of knob values for the scoring fields.
    // `vm_lookback_minutes` arrives via the same atomic snapshot from
    // `measurements_for_campaign` (returned alongside `inputs`) so the
    // VM fetch window is consistent with the scoring knobs even under
    // a racing PATCH.
    let loss_threshold_ratio = inputs.loss_threshold_ratio;
    let stddev_weight = inputs.stddev_weight;
    let evaluation_mode = inputs.mode;
    let max_transit_rtt_ms = inputs.max_transit_rtt_ms;
    let max_transit_stddev_ms = inputs.max_transit_stddev_ms;
    let min_improvement_ms = inputs.min_improvement_ms;
    let min_improvement_ratio = inputs.min_improvement_ratio;
    let useful_latency_ms = inputs.useful_latency_ms;
    let max_hops = inputs.max_hops;

    // Hoist reverse-direction measurements above the mode branch.
    // The symmetry-fallback in `LegLookup` uses these rows for both
    // Triple and EdgeCandidate modes — collecting them once here keeps
    // the per-mode branches lean.
    let reverse_measurements =
        match repo::reverse_direction_measurements_for_campaign(&state.pool, id).await {
            Ok(r) => r,
            Err(e) => return repo_error("campaign::evaluate::reverse", e),
        };
    if !reverse_measurements.is_empty() {
        // Reverse rows are appended AFTER the active-probe set so that
        // `build_pair_lookup`'s last-write-wins semantics preserve the
        // forward direction whenever both directions are present. The
        // `LegLookup` symmetry path picks up these rows for the legs
        // where only the reverse direction was measured.
        inputs.measurements.extend(reverse_measurements);
    }

    // Validate EdgeCandidate preconditions before the expensive VM fetch.
    if evaluation_mode == EvaluationMode::EdgeCandidate {
        // A campaign with no destination IPs in `campaign_pairs` can't
        // produce any meaningful edge-pair output. The schema enforces
        // at least one destination_ip at create time, but an `/edit`
        // that removes all pairs could leave the campaign in this state.
        if inputs.candidate_ips.is_empty() {
            return error_422("no_destinations").into_response();
        }
    }

    // Layer VM continuous-mesh baselines on top of the active-probe rows
    // for agent→agent pairs the campaign did not cover.
    // `fetch_and_synthesize_vm_baselines` returns rows to prepend so
    // active-probe data wins on `build_pair_lookup`'s last-write-wins
    // (see `eval::evaluate`'s `by_pair` loop). The campaign's
    // `vm_lookback_minutes` knob drives the PromQL `[Xm]` window.
    //
    // Pass the FULL identity roster (`inputs.roster`), not the
    // source-agent subset (`inputs.agents`). For EdgeCandidate the
    // candidate X may be a mesh agent that is not a campaign source;
    // its outgoing X→B baseline still has to be fetched from VM so the
    // evaluator's `LegLookup` can resolve the direct leg without
    // falling back to a reverse-direction substitution. For Diversity /
    // Optimization `roster == agents` (the C3-8 invariant), so behavior
    // for those modes is unchanged.
    let vm_url_opt = state
        .config()
        .upstream
        .vm_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_owned());
    match fetch_and_synthesize_vm_baselines(
        vm_url_opt.as_deref(),
        campaign.protocol,
        &inputs.measurements,
        &inputs.roster,
        vm_lookback_minutes,
    )
    .await
    {
        Ok(synthesized) => {
            if !synthesized.is_empty() {
                let mut combined =
                    Vec::with_capacity(synthesized.len() + inputs.measurements.len());
                // Synthesized FIRST so the `by_pair` loop in the evaluator
                // overwrites any VM row with a matching active-probe row —
                // "active wins over VM" when both are present.
                combined.extend(synthesized);
                combined.append(&mut inputs.measurements);
                inputs.measurements = combined;
            }
        }
        Err(VmQueryError::NotConfigured) => {
            // `fetch_and_synthesize_vm_baselines` short-circuits to
            // `Ok(Vec::new())` when `vm_url_opt` is `None` and
            // `vm_query::fetch_agent_baselines` itself never produces
            // `NotConfigured` (see `crates/service/src/vm_query.rs`).
            // Panic so any future refactor that lets this variant
            // escape fails loudly in tests rather than being silently
            // dropped and masking a regression.
            unreachable!(
                "VmQueryError::NotConfigured is structurally unreachable here: \
                 caller gates on Some(vm_url) before dispatching the fetch"
            );
        }
        Err(
            e @ (VmQueryError::UpstreamStatus(_)
            | VmQueryError::Request(_)
            | VmQueryError::MalformedResponse(_)),
        ) => {
            tracing::warn!(
                campaign_id = %id,
                error = %e,
                "evaluate: VM baseline fetch failed; surfacing 503 vm_upstream"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "vm_upstream",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    }

    // EdgeCandidate: reject when there are no measurements at all after
    // combining active-probe + reverse + VM rows. Without data the
    // evaluator would produce zero candidates, which is misleading —
    // a 422 here prompts the operator to check agent connectivity.
    if evaluation_mode == EvaluationMode::EdgeCandidate && inputs.measurements.is_empty() {
        return error_422("no_candidates_with_data").into_response();
    }

    let outputs = match eval::evaluate(inputs) {
        Ok(o) => o,
        // EdgeCandidate evaluator is infallible; this arm only fires for Triple mode.
        Err(EvalError::NoBaseline) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "no_baseline_pairs" })),
            )
                .into_response();
        }
    };

    // `persist_evaluation` writes the parent `campaign_evaluations`
    // row and every child `campaign_evaluation_{candidates,
    // pair_details, unqualified_reasons}` row inside one transaction,
    // then promotes `completed → evaluated`. A crash between the
    // insert and the state flip would otherwise leave the campaign
    // stuck in `completed` with a written evaluation history row —
    // inconsistent for UI consumers keyed off `state`.
    if let Err(e) = evaluation_repo::persist_evaluation(
        &state.pool,
        id,
        &outputs,
        loss_threshold_ratio,
        stddev_weight,
        evaluation_mode,
        max_transit_rtt_ms,
        max_transit_stddev_ms,
        min_improvement_ms,
        min_improvement_ratio,
        useful_latency_ms,
        max_hops,
        vm_lookback_minutes,
    )
    .await
    {
        return repo_error("campaign::evaluate::persist", e);
    }

    // Re-read the latest row so the response carries the exact
    // relational shape the read-path assembles (ordering, composite
    // score recompute, hostname stamp surface). The cost is one extra
    // index lookup; the alternative is threading the just-written
    // parent row through the orchestrator, duplicating assembly
    // logic.
    let mut dto = match evaluation_repo::latest_evaluation_for_campaign(&state.pool, id).await {
        Ok(Some(dto)) => dto,
        Ok(None) => {
            tracing::error!(
                %id,
                "campaign::evaluate: freshly written row missing on read-back"
            );
            return db_error("campaign::evaluate::readback", sqlx::Error::RowNotFound);
        }
        Err(e) => return db_error("campaign::evaluate::readback", e),
    };

    // No in-process broker publish here. The `campaign_evaluations_notify`
    // trigger fires `campaign_evaluated` inside the same transaction, and
    // the campaign SSE listener fans that out to every subscriber (this
    // instance's and its peers') — a direct publish here would cause
    // same-instance clients to receive a duplicate `evaluated` frame.

    let session = session_id_from_auth(&auth);
    stamp_evaluation_dto(&state, &session, &mut dto).await;
    (StatusCode::OK, Json(dto)).into_response()
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
    auth: AuthSession,
    Path(id): Path<Uuid>,
) -> Response {
    match evaluation_repo::latest_evaluation_for_campaign(&state.pool, id).await {
        Ok(Some(mut dto)) => {
            let session = session_id_from_auth(&auth);
            stamp_evaluation_dto(&state, &session, &mut dto).await;
            (StatusCode::OK, Json(dto)).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "not_evaluated" })),
        )
            .into_response(),
        Err(e) => db_error("campaign::get_evaluation", e),
    }
}

/// `GET /api/campaigns/{id}/evaluation/edge_pairs` — paginated list of
/// per-(X, B) edge-pair detail rows from the campaign's most recent
/// EdgeCandidate evaluation.
///
/// Supports server-side sort (8 sort columns × 2 directions),
/// runtime filters (`candidate_ip`, `qualifies_only`, `reachable_only`),
/// and an opaque keyset cursor for forward pagination. Page size is capped
/// at 500 rows; the default is 100.
///
/// Note: the plan's `qualifies_first` compound sort and
/// `coverage_weighted_ping_*` sorts are deferred (need a JOIN to the
/// candidates table) — record as a future enhancement.
// TODO: add destination_agent_id filter as a follow-up.
///
/// Error vocabulary:
/// - `not_evaluated` (404): the campaign has never been evaluated.
/// - `wrong_mode` (404): the campaign's latest evaluation is not
///   `edge_candidate` mode.
/// - `invalid_sort` (400): `sort` value is not one of the whitelisted names.
/// - `invalid_candidate_ip` (400): `candidate_ip` query param is present
///   but can't be parsed as an IP address.
/// - `invalid_filter` (400): `limit` exceeds the 500-row cap.
/// - `invalid_cursor` (400): cursor is undecodable or its sort column
///   doesn't match the request's `sort`.
#[utoipa::path(
    get,
    path = "/api/campaigns/{id}/evaluation/edge_pairs",
    tag = "campaigns",
    params(
        ("id" = Uuid, Path, description = "Campaign id"),
        EdgePairsQuery,
    ),
    responses(
        (status = 200, description = "Edge-pair page", body = EdgePairsListResponse),
        (status = 400, description = "invalid_sort | invalid_candidate_ip | invalid_filter | invalid_cursor", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 404, description = "not_evaluated | wrong_mode", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn get_edge_pairs(
    State(state): State<AppState>,
    _auth: AuthSession,
    Path(id): Path<Uuid>,
    Query(query): Query<EdgePairsQuery>,
) -> Response {
    // Validate limit cap before any DB round-trip.
    const EDGE_PAIRS_MAX_LIMIT: u32 = 500;
    if query.limit > EDGE_PAIRS_MAX_LIMIT {
        return error_400("invalid_filter").into_response();
    }

    // Validate candidate_ip filter format up-front.
    if let Some(ref raw) = query.candidate_ip {
        if raw.parse::<IpAddr>().is_err() {
            return error_400("invalid_candidate_ip").into_response();
        }
    }

    // Cursor validation runs before the DB round-trip so a stale page-2
    // cursor surfaces as 400 without consuming a pool connection.
    if let Some(raw) = &query.cursor {
        if let Err(err) = super::dto::EdgePairCursor::decode(raw, query.sort) {
            let error_kind = match err {
                CursorError::Decode(_) => "decode",
                CursorError::SortMismatch => "sort_mismatch",
                CursorError::InvalidEnum => "invalid_enum",
            };
            tracing::debug!(
                error_kind = %error_kind,
                %err,
                "campaign::edge_pairs: invalid cursor",
            );
            return error_400("invalid_cursor").into_response();
        }
    }

    match evaluation_repo::latest_evaluation_edge_pairs(&state.pool, id, &query).await {
        Ok(EdgePairLookup::Found(page)) => {
            // `destination_hostname` is left as `None` here; the frontend
            // resolves it via the existing `IpHostname` component using the
            // `destination_agent_id`. Resolving it server-side requires a
            // SQL lookup of `agents.ip` by id before hitting the hostname
            // cache — deferred as a follow-up enhancement.
            (StatusCode::OK, Json(page)).into_response()
        }
        Ok(EdgePairLookup::CampaignNotFound) => {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
        }
        Ok(EdgePairLookup::NoEvaluation) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "not_evaluated" })),
        )
            .into_response(),
        Ok(EdgePairLookup::WrongMode) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "wrong_mode" })),
        )
            .into_response(),
        Err(e) => db_error("campaign::get_edge_pairs", e),
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
/// - `good_candidates`: mode-aware — pulls the persisted evaluation and
///   expands "good" candidates into `(source_agent, destination_ip)` pairs:
///   - **Triple modes** (Diversity / Optimization): expands each qualifying
///     `(source_agent, destination_agent, transit_ip)` triple into a pair of
///     `(source_agent → transit_ip)` + `(destination_agent → transit_ip)`.
///   - **EdgeCandidate**: selects candidates with `coverage_count ≥ 1` and
///     creates `(source_agent, candidate_ip)` pairs for each qualifying
///     destination. Requires a prior evaluation; 400 `no_evaluation` otherwise.
/// - `pair`: a single operator-chosen `(source_agent_id,
///   destination_ip)` tuple from the request body.
///
/// All scopes flow through [`repo::insert_detail_pairs`], which owns
/// the transition back to `running` and emits `campaign_state_changed`
/// through the Postgres NOTIFY trigger. The campaign SSE listener
/// translates that NOTIFY into a broker broadcast for every
/// subscriber (same-instance and peers) — this handler does not
/// publish to the in-process broker directly, to avoid sending a
/// duplicate `state_changed` frame on the request's own instance.
/// No-op detail inserts (inserted == 0 or an already-`running` race)
/// don't touch `state`, so the trigger stays silent correctly.
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

    // Scope contract (see `DetailRequest::pair` docstring): `pair` is
    // required iff scope=='pair' and must be absent otherwise.
    // Silently ignoring an extraneous payload hides client bugs.
    if body.pair.is_some() && !matches!(body.scope, DetailScope::Pair) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unexpected_pair_payload" })),
        )
            .into_response();
    }

    let pairs: Vec<(String, IpAddr)> = match body.scope {
        DetailScope::All => match repo::settled_campaign_pairs(&state.pool, id).await {
            Ok(p) => p,
            Err(e) => return repo_error("campaign::detail::all", e),
        },
        DetailScope::GoodCandidates => {
            // Tighten the state gate for this scope: only `evaluated`
            // is valid. The outer gate admits `completed` too so that
            // `/detail?scope=all` keeps working, but a `completed`
            // campaign that carries an old `campaign_evaluations` row
            // (from a prior run preserved across `apply_edit` /
            // `force_pair` / re-run flows) must NOT expand that stale
            // candidate set into fresh detail pairs — the candidates
            // might target IPs that are no longer in the pair graph.
            // `no_evaluation` covers both "no row yet" and "row exists
            // but is not fresh for the current run state"; either way
            // the operator fix is an explicit re-`/evaluate`.
            if !matches!(campaign.state, CampaignState::Evaluated) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "no_evaluation" })),
                )
                    .into_response();
            }

            if campaign.evaluation_mode == EvaluationMode::EdgeCandidate {
                // EdgeCandidate mode: "good" candidates are those with
                // `coverage_count >= 1` (at least one destination agent
                // B was reachable under the latency threshold T). For
                // each qualifying candidate X, create
                // `(source_agent, X)` pairs using the campaign's settled
                // source agents — the same agents that probed X during
                // the campaign run.
                let edge_pairs =
                    match evaluation_repo::good_candidates_for_edge_campaign(&state.pool, id).await
                    {
                        Ok(Some(rows)) => rows,
                        Ok(None) => {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({ "error": "no_evaluation" })),
                            )
                                .into_response();
                        }
                        Err(e) => return db_error("campaign::detail::edge_eval", e),
                    };
                let mut acc = edge_pairs;
                acc.sort();
                acc.dedup();
                acc
            } else {
                // Triple modes (Diversity / Optimization): expand each
                // qualifying `(A, B, X)` triple into `(A, X)` and
                // `(B, X)` measurement targets so both legs of the
                // transit route get a higher-resolution probe.
                let legs = match evaluation_repo::good_candidate_pair_legs(&state.pool, id).await {
                    Ok(Some(rows)) => rows,
                    Ok(None) => {
                        // Defensive: `state=evaluated` implies a row exists
                        // (persist_evaluation is the only writer that sets
                        // that state). Reaching this arm would mean a
                        // concurrent DELETE raced the read; treat as
                        // missing evaluation.
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({ "error": "no_evaluation" })),
                        )
                            .into_response();
                    }
                    Err(e) => return db_error("campaign::detail::eval", e),
                };
                let mut acc: Vec<(String, IpAddr)> = Vec::new();
                for leg in &legs {
                    acc.push((leg.source_agent_id.clone(), leg.candidate_destination_ip));
                    acc.push((
                        leg.destination_agent_id.clone(),
                        leg.candidate_destination_ip,
                    ));
                }
                acc.sort();
                acc.dedup();
                acc
            }
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

    let (enqueued, post_state) = match repo::insert_detail_pairs(&state.pool, id, &pairs).await {
        Ok(out) => out,
        Err(e) => return repo_error("campaign::detail::insert", e),
    };

    // No in-process broker publish here. The `measurement_campaigns_notify`
    // trigger fires `campaign_state_changed` inside the same transaction
    // as the `Completed|Evaluated → Running` flip, and the campaign SSE
    // listener fans that out to every subscriber (this instance and
    // peers alike) — a direct publish would cause same-instance clients
    // to receive a duplicate `state_changed` frame for one transition.
    // No-flip paths (inserted==0 or already-running race) don't fire
    // the trigger, so the listener stays silent correctly.

    (
        StatusCode::OK,
        Json(DetailResponse {
            pairs_enqueued: enqueued,
            // `post_state` is read back inside the insert transaction's
            // lock, so it reflects the campaign's actual state at
            // commit time (not the stale pre-read).
            campaign_state: post_state,
        }),
    )
        .into_response()
}

/// Maximum `limit` accepted by the paginated pair_details endpoint.
/// Exceeding the cap surfaces as `400 invalid_filter` — the wire is
/// rate-limited by `total` rather than per-request size.
const PAIR_DETAILS_MAX_LIMIT: u32 = 500;

/// `GET /api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details`
/// — paginated detail breakdown of a single transit candidate's
/// per-baseline-pair scoring rows.
///
/// Replaces the unbounded `EvaluationCandidateDto.pair_details` array
/// the wire used to ship inline. The endpoint applies server-side sort
/// (10-column whitelist), four optional runtime filters
/// (`min_improvement_ms`, `min_improvement_ratio`, `max_transit_rtt_ms`,
/// `max_transit_stddev_ms`) plus `qualifies_only`, and an opaque keyset
/// cursor for forward pagination. The cursor's tiebreak rides on the
/// composite primary key `(source_agent_id, destination_agent_id)`,
/// which is unique within a single
/// `(evaluation_id, candidate_destination_ip)` tuple.
///
/// Error vocabulary:
/// - `not_found` (404): the campaign id does not exist.
/// - `no_evaluation` (404): the campaign has never been evaluated.
/// - `not_a_candidate` (404): the latest evaluation does not include
///   `destination_ip` as a candidate.
/// - `invalid_filter` (400): `limit > 500`, or any filter value is
///   non-finite (`NaN` / `Infinity`).
/// - `invalid_cursor` (400): cursor undecodable, or its sort column
///   does not match the request's `sort` parameter.
/// - `invalid_sort` (400): `sort` is not one of the whitelisted columns
///   (handled by serde at deserialization time).
#[utoipa::path(
    get,
    path = "/api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details",
    tag = "campaigns",
    params(
        ("id" = Uuid, Path, description = "Campaign id"),
        ("destination_ip" = String, Path, description = "Transit candidate destination IP"),
        EvaluationPairDetailQuery,
    ),
    responses(
        (status = 200, description = "Pair-detail page", body = EvaluationPairDetailListResponse),
        (status = 400, description = "Invalid filter / cursor / sort", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 404, description = "Campaign / evaluation / candidate not found", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn get_candidate_pair_details(
    State(state): State<AppState>,
    auth: AuthSession,
    Path((id, destination_ip)): Path<(Uuid, String)>,
    Query(query): Query<EvaluationPairDetailQuery>,
) -> Response {
    // Validate up-front so the SQL planner never sees a NaN threshold
    // and the cursor decode happens before the (cheap) campaign /
    // evaluation existence checks.
    if query.limit > PAIR_DETAILS_MAX_LIMIT {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_filter" })),
        )
            .into_response();
    }
    let finite_filters_ok = [
        query.min_improvement_ms,
        query.min_improvement_ratio,
        query.max_transit_rtt_ms,
        query.max_transit_stddev_ms,
    ]
    .iter()
    .all(|v| v.map(|x| x.is_finite()).unwrap_or(true));
    if !finite_filters_ok {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_filter" })),
        )
            .into_response();
    }

    // Cursor validation runs before the DB roundtrip so a stale page-2
    // cursor with the wrong sort surfaces as 400 without consuming a
    // pool connection. Every [`CursorError`] variant maps to the same
    // `invalid_cursor` wire code; the discriminant is captured as a
    // structured `error_kind` field on the debug log for telemetry.
    if let Some(raw) = &query.cursor {
        if let Err(err) = PairDetailCursor::decode(raw, query.sort) {
            let error_kind = match err {
                CursorError::Decode(_) => "decode",
                CursorError::SortMismatch => "sort_mismatch",
                CursorError::InvalidEnum => "invalid_enum",
            };
            tracing::debug!(
                error_kind = %error_kind,
                %err,
                "campaign::pair_details: invalid cursor",
            );
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_cursor" })),
            )
                .into_response();
        }
    }

    let dest_ip = match IpAddr::from_str(&destination_ip) {
        Ok(ip) => ip,
        Err(_) => {
            // Path segment didn't parse as an IP — surface the same
            // 404 the candidate-not-found path returns. The candidate
            // table can only carry `INET` rows, so an unparseable
            // string can never match anything anyway.
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "not_a_candidate" })),
            )
                .into_response();
        }
    };

    match evaluation_repo::latest_pair_details_for_candidate(&state.pool, id, dest_ip, &query).await
    {
        Ok(PairDetailLookup::Found {
            mut entries,
            total,
            next_cursor,
        }) => {
            // Stamp `destination_hostname` on every entry. Each row's
            // `destination_ip` mirrors the candidate transit IP (`X`),
            // so a single bulk lookup keyed on the path-segment IP
            // covers every entry on the page. Non-fatal: on DB error,
            // log a warning and emit the page without hostnames.
            if !entries.is_empty() {
                let session = session_id_from_auth(&auth);
                match bulk_hostnames_and_enqueue(&state, &session, &[dest_ip]).await {
                    Ok(map) => {
                        if let Some(Some(h)) = map.get(&dest_ip) {
                            for entry in entries.iter_mut() {
                                entry.destination_hostname = Some(h.clone());
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "campaign::pair_details: hostname stamp failed; returning unhostnamed page"
                        );
                    }
                }
            }
            (
                StatusCode::OK,
                Json(EvaluationPairDetailListResponse {
                    entries,
                    total,
                    next_cursor,
                }),
            )
                .into_response()
        }
        Ok(PairDetailLookup::CampaignNotFound) => {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
        }
        Ok(PairDetailLookup::NoEvaluation) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no_evaluation" })),
        )
            .into_response(),
        Ok(PairDetailLookup::NotACandidate) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "not_a_candidate" })),
        )
            .into_response(),
        Err(e) => db_error("campaign::get_candidate_pair_details", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_edge_candidate_useful_latency_nan_returns_error() {
        // NaN should be rejected by is_nan() check
        let result = validate_create_or_patch_knobs(
            super::super::model::EvaluationMode::EdgeCandidate,
            Some(f32::NAN),
            Some(2),
            Some(15),
        );

        assert!(result.is_err(), "NaN should be rejected");
        let (status, body) = result.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let error = serde_json::to_value(&body.0)
            .ok()
            .and_then(|v| v["error"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "parse_error".to_string());
        assert_eq!(error, "useful_latency_invalid");
    }

    #[test]
    fn validate_diversity_useful_latency_nan_returns_error() {
        // NaN should also be rejected in diversity mode
        let result = validate_create_or_patch_knobs(
            super::super::model::EvaluationMode::Diversity,
            Some(f32::NAN),
            Some(1),
            Some(15),
        );

        assert!(result.is_err(), "NaN should be rejected in diversity mode");
        let (status, _body) = result.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
