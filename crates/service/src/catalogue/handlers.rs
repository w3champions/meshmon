//! HTTP handlers for the operator-facing catalogue surface.
//!
//! - `POST /api/catalogue` — paste-and-insert. Parses tokens, inserts
//!   the accepted IPs, splits the result into created / existing /
//!   invalid buckets and enqueues each newly-created id for enrichment.
//! - `GET /api/catalogue` — filtered list. Multi-valued filters use ANY
//!   semantics (see [`super::dto::ListQuery`]); cursor pagination is
//!   deferred to T13.
//! - `GET /api/catalogue/{id}` — single-row fetch.
//!
//! Every handler lives behind the `login_required!` gate via
//! [`crate::http::openapi::api_router`] wiring; anonymous callers
//! short-circuit with 401 before the handler runs. PATCH / DELETE are
//! T12's scope; re-enrich + facets land in T13; the SSE route + OpenAPI
//! registration consolidate in T16.

use super::dto::{
    CatalogueEntryDto, ErrorEnvelope, ListQuery, ListResponse, PasteInvalid, PasteRequest,
    PasteResponse,
};
use super::events::CatalogueEvent;
use super::model::CatalogueSource;
use super::parse::{parse_ip_tokens, ParseReason};
use super::repo;
use crate::http::auth::AuthSession;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use uuid::Uuid;

/// Render a repo error as an HTTP 500 with a stable JSON error body.
///
/// Logs the full error server-side and hides the internal message from
/// the client so we never leak sqlx-level detail onto the wire. Shared
/// helper so every handler emits the same shape.
fn db_error(context: &'static str, err: sqlx::Error) -> Response {
    tracing::error!(error = %err, "{context}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "database_error" })),
    )
        .into_response()
}

/// Turn a parse [`ParseReason`] into a short operator-friendly string.
fn reason_label(reason: ParseReason) -> String {
    match reason {
        ParseReason::InvalidIp(_) => "invalid_ip".to_string(),
        ParseReason::CidrNotAllowed { prefix_len } => {
            format!("cidr_not_allowed:/{prefix_len}")
        }
    }
}

/// `POST /api/catalogue` — operator paste flow.
///
/// Tokens are concatenated with spaces and run through
/// [`parse_ip_tokens`]; accepted IPs become catalogue rows via
/// [`repo::insert_many`], and each newly-created id is enqueued for
/// enrichment. Existing rows come back under `existing` so the UI can
/// surface their current enrichment state without a follow-up fetch.
/// Rejected tokens land in `invalid`.
#[utoipa::path(
    post,
    path = "/api/catalogue",
    tag = "catalogue",
    request_body = PasteRequest,
    responses(
        (status = 200, description = "Paste outcome", body = PasteResponse),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn paste(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Json(body): Json<PasteRequest>,
) -> Response {
    // `login_required!` guarantees authentication before the handler
    // runs; `None` here would imply a router-wiring regression. Mirror
    // the defensive pattern used in `http::session::session`.
    let Some(principal) = auth_session.user.as_ref() else {
        return (StatusCode::UNAUTHORIZED, "not authenticated").into_response();
    };

    // `parse_ip_tokens` accepts any delimiter; join the array so the
    // parser sees one blob regardless of how the client split tokens.
    // T11 does not expose `parsed.duplicates` on the wire; parse.rs
    // computes them for the future T12/T13 frontend badge. Intentional
    // drop — do not re-add the allocation thinking it's a bug.
    let joined = body.ips.join(" ");
    let parsed = parse_ip_tokens(&joined);

    let outcome = match repo::insert_many(
        &state.pool,
        &parsed.accepted,
        CatalogueSource::Operator,
        Some(&principal.username),
    )
    .await
    {
        Ok(o) => o,
        Err(e) => return db_error("catalogue paste: insert_many failed", e),
    };

    // Publish `Created` per inserted row and enqueue each for
    // enrichment. Enqueue failures are intentionally silent under T11
    // — the runner's periodic sweep picks up any row whose queue push
    // missed once `created_at` crosses the staleness threshold.
    for row in &outcome.created {
        state.catalogue_broker.publish(CatalogueEvent::Created {
            id: row.id,
            ip: row.ip.to_string(),
        });
        let _ = state.enrichment_queue.enqueue(row.id);
    }

    let invalid = parsed
        .rejected
        .into_iter()
        .map(|(token, reason)| PasteInvalid {
            token,
            reason: reason_label(reason),
        })
        .collect();

    let response = PasteResponse {
        created: outcome
            .created
            .into_iter()
            .map(CatalogueEntryDto::from)
            .collect(),
        existing: outcome
            .existing
            .into_iter()
            .map(CatalogueEntryDto::from)
            .collect(),
        invalid,
    };
    (StatusCode::OK, Json(response)).into_response()
}

/// `GET /api/catalogue` — filtered, size-bounded list.
///
/// The handler converts [`ListQuery`] straight into [`repo::ListFilter`]
/// and returns a [`ListResponse`]. Cursor pagination is accepted on the
/// wire but ignored until T13 — the repo implementation clamps the
/// response to the first `limit.min(500)` rows in
/// `(created_at DESC, id DESC)` order.
#[utoipa::path(
    get,
    path = "/api/catalogue",
    tag = "catalogue",
    params(ListQuery),
    responses(
        (status = 200, description = "Catalogue page", body = ListResponse),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    let filter = repo::ListFilter {
        country_code: q.country_code,
        asn: q.asn,
        network: q.network,
        ip_prefix: q.ip_prefix,
        name: q.name,
        bounding_box: q.bbox,
        limit: q.limit,
        cursor_created_at: q.cursor_created_at,
        cursor_id: q.cursor_id,
    };
    match repo::list(&state.pool, filter).await {
        Ok((entries, total)) => {
            let body = ListResponse {
                entries: entries.into_iter().map(CatalogueEntryDto::from).collect(),
                total,
            };
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(e) => db_error("catalogue list: repo::list failed", e),
    }
}

/// `GET /api/catalogue/{id}` — single-row lookup.
///
/// Returns 404 with a JSON error body when the id is absent. Keeping
/// the 404 body parallel to [`crate::http::user_api::get_agent`] so
/// SPA error handling stays uniform across catalogue + registry.
#[utoipa::path(
    get,
    path = "/api/catalogue/{id}",
    tag = "catalogue",
    params(
        ("id" = Uuid, Path, description = "Catalogue row id"),
    ),
    responses(
        (status = 200, description = "Catalogue row", body = CatalogueEntryDto),
        (status = 401, description = "No active session"),
        (status = 404, description = "Row not found", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn get_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match repo::find_by_id(&state.pool, id).await {
        Ok(Some(row)) => (StatusCode::OK, Json(CatalogueEntryDto::from(row))).into_response(),
        // Body key matches the gateway's `backend_path_404` envelope and
        // the 500 path's `db_error` helper — every non-2xx `/api` response
        // carries a snake_case, machine-parseable `error` code.
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response(),
        Err(e) => db_error("catalogue get_one: find_by_id failed", e),
    }
}
