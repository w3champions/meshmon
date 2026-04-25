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
    DetailScope, EditCampaignRequest, EditPairDto, ErrorEnvelope, EvaluationDto,
    EvaluationPairDetailListResponse, EvaluationPairDetailQuery, ForcePairRequest, PairDto,
    PairListQuery, PatchCampaignRequest, PreviewDispatchResponse,
};
use super::eval::{self, AttributedMeasurement, EvalError};
use super::evaluation_repo::{self, PairDetailLookup};
use super::model::{CampaignState, DirectSource, PairResolutionState, ProbeProtocol};
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

/// Lookback window for VM continuous-mesh baselines surfaced into the
/// evaluator at `/evaluate` time. 15 minutes matches the operator
/// expectation of a "recent" baseline without inflating the `avg_over_time`
/// aggregation cost — VM samples tick every ~30 s per pair.
const VM_BASELINE_LOOKBACK: Duration = Duration::from_secs(15 * 60);

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
        loss_threshold_ratio: body.loss_threshold_ratio,
        stddev_weight: body.stddev_weight,
        evaluation_mode: body.evaluation_mode,
        max_transit_rtt_ms: body.max_transit_rtt_ms,
        max_transit_stddev_ms: body.max_transit_stddev_ms,
        min_improvement_ms: body.min_improvement_ms,
        min_improvement_ratio: body.min_improvement_ratio,
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
        body.loss_threshold_ratio,
        body.stddev_weight,
        body.evaluation_mode,
        body.max_transit_rtt_ms,
        body.max_transit_stddev_ms,
        body.min_improvement_ms,
        body.min_improvement_ratio,
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

/// Stamp `hostname` on [`EvaluationDto`] candidates and
/// `destination_hostname` on their nested `pair_details`.
///
/// Collects all destination IPs from the full nested structure, bulk-
/// resolves in one DB round-trip, and writes matched hostnames back.
/// Non-fatal: on DB error, logs at `warn!` and returns without hostnames.
async fn stamp_evaluation_dto(
    state: &AppState,
    session: &crate::hostname::SessionId,
    dto: &mut EvaluationDto,
) {
    // Collect all unique IPs in one flat pass.
    let ips: Vec<IpAddr> = dto
        .results
        .candidates
        .iter()
        .flat_map(|c| {
            let cand_ip = c.destination_ip.parse::<IpAddr>().ok();
            let detail_ips = c
                .pair_details
                .iter()
                .filter_map(|pd| pd.destination_ip.parse::<IpAddr>().ok());
            cand_ip.into_iter().chain(detail_ips)
        })
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
                for pd in cand.pair_details.iter_mut() {
                    if let Ok(ip) = pd.destination_ip.parse::<IpAddr>() {
                        if let Some(Some(h)) = map.get(&ip) {
                            pd.destination_hostname = Some(h.clone());
                        }
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
    let samples = vm_query::fetch_agent_baselines(
        vm_url,
        &roster_ids,
        protocol_label(protocol),
        VM_BASELINE_LOOKBACK,
    )
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
/// Returns:
/// * 422 (`no_baseline_pairs`) — no agent→agent baseline available,
///   even after the VM fallback (or VM wasn't configured).
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
        (status = 422, description = "No baseline (agent→agent) pairs", body = ErrorEnvelope),
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

    let mut inputs = match repo::measurements_for_campaign(&state.pool, id).await {
        Ok(i) => i,
        Err(e) => return repo_error("campaign::evaluate::inputs", e),
    };
    // Snapshot the evaluator knobs from `inputs` (not `campaign`) so a
    // concurrent `PATCH /campaigns/{id}` that lands between the two reads
    // cannot desync the scored knobs from the persisted evaluation row.
    // `campaign` stays in use only for its `state` gate above; it is no
    // longer the source of knob values.
    let loss_threshold_ratio = inputs.loss_threshold_ratio;
    let stddev_weight = inputs.stddev_weight;
    let evaluation_mode = inputs.mode;
    let max_transit_rtt_ms = inputs.max_transit_rtt_ms;
    let max_transit_stddev_ms = inputs.max_transit_stddev_ms;
    let min_improvement_ms = inputs.min_improvement_ms;
    let min_improvement_ratio = inputs.min_improvement_ratio;

    // T54-03: layer VM continuous-mesh baselines on top of the active-
    // probe rows for agent→agent pairs the campaign did not cover.
    // `fetch_and_synthesize_vm_baselines` returns the rows to prepend so
    // active-probe data wins on `HashMap::insert` (last write wins; see
    // `eval::evaluate`'s `by_pair` loop).
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
        &inputs.agents,
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
            let dto = match evaluation_repo::latest_evaluation_for_campaign(&state.pool, id).await {
                Ok(Some(r)) => r,
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
            for cand in dto
                .results
                .candidates
                .iter()
                .filter(|c| c.pairs_improved >= 1)
            {
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
/// post-T54 composite primary key
/// `(source_agent_id, destination_agent_id)`, which is unique within a
/// single `(evaluation_id, candidate_destination_ip)` tuple.
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
    _auth: AuthSession,
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
            entries,
            total,
            next_cursor,
        }) => (
            StatusCode::OK,
            Json(EvaluationPairDetailListResponse {
                entries,
                total,
                next_cursor,
            }),
        )
            .into_response(),
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
