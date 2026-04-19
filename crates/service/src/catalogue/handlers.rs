//! HTTP handlers for the operator-facing catalogue surface.
//!
//! - `POST /api/catalogue` — paste-and-insert. Parses tokens, inserts
//!   the accepted IPs, splits the result into created / existing /
//!   invalid buckets and enqueues each newly-created id for enrichment.
//! - `GET /api/catalogue` — filtered list. Multi-valued filters use ANY
//!   semantics (see [`super::dto::ListQuery`]); cursor pagination is
//!   deferred to T13.
//! - `GET /api/catalogue/{id}` — single-row fetch.
//! - `PATCH /api/catalogue/{id}` — partial update with revert-to-auto
//!   support. Touched fields flip into `operator_edited_fields`; names
//!   listed in `revert_to_auto` are NULLed and removed from the lock
//!   set so the next enrichment run re-populates them.
//! - `DELETE /api/catalogue/{id}` — idempotent removal.
//!
//! Every handler lives behind the `login_required!` gate via
//! [`crate::http::openapi::api_router`] wiring; anonymous callers
//! short-circuit with 401 before the handler runs. Re-enrich + facets
//! land in T13; the SSE route + OpenAPI registration consolidate in T16.

use super::dto::{
    CatalogueEntryDto, ErrorEnvelope, ListQuery, ListResponse, PasteInvalid, PasteRequest,
    PasteResponse, PatchRequest,
};
use super::events::CatalogueEvent;
use super::model::{CatalogueSource, Field};
use super::parse::{parse_ip_tokens, ParseReason};
use super::repo;
use crate::http::auth::AuthSession;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::str::FromStr;
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

/// `PATCH /api/catalogue/{id}` — partial update with revert-to-auto.
///
/// Each supplied field writes through [`repo::patch`], which also
/// appends the corresponding [`Field`] to `operator_edited_fields` so
/// downstream enrichment skips it. Names listed in
/// [`PatchRequest::revert_to_auto`] are parsed via
/// [`Field::from_str`](std::str::FromStr::from_str) (unknown strings
/// silently dropped, matching the list endpoint's permissive filter
/// semantics), NULLed in the corresponding column, and removed from the
/// lock set so the next enrichment run re-populates them. A successful
/// update publishes [`CatalogueEvent::Updated`].
///
/// Revert-vs-set conflict: if both `city: Some(Some("X"))` and
/// `revert_to_auto: ["City"]` are sent in the same PATCH, the revert
/// wins and the write is suppressed — matching [`repo::patch`]'s
/// documented semantics. See also [`repo::patch`] for the SQL-level
/// precedence rule.
#[utoipa::path(
    patch,
    path = "/api/catalogue/{id}",
    tag = "catalogue",
    params(
        ("id" = Uuid, Path, description = "Catalogue row id"),
    ),
    request_body = PatchRequest,
    description = "Partial update with revert-to-auto support. If a field is \
        present in both the body and `revert_to_auto`, the revert wins and \
        the write is suppressed (matches repo::patch semantics).",
    responses(
        (status = 200, description = "Updated row", body = CatalogueEntryDto),
        (status = 401, description = "No active session"),
        (status = 404, description = "Row not found", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn patch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchRequest>,
) -> Response {
    let revert_to_auto: Vec<Field> = req
        .revert_to_auto
        .iter()
        .filter_map(|s| Field::from_str(s).ok())
        .collect();

    // Revert-vs-set conflict: if a field name appears in both the body
    // and `revert_to_auto`, the revert wins and the write is suppressed
    // at the handler level. The suppression matters because
    // `repo::patch` adds every `Some(_)` field to `operator_edited_fields`
    // AFTER subtracting the removed names — so without dropping the set
    // here, the lock would be silently re-added alongside the NULLed
    // value. The SQL's CASE clause already ensures the column value ends
    // up NULL; this guard keeps the lock set consistent with that.
    fn suppress_if_reverted<T>(
        field: Field,
        value: repo::PatchValue<T>,
        revert: &[Field],
    ) -> repo::PatchValue<T> {
        if revert.contains(&field) {
            None
        } else {
            value
        }
    }
    let patch_set = repo::PatchSet {
        display_name: suppress_if_reverted(Field::DisplayName, req.display_name, &revert_to_auto),
        city: suppress_if_reverted(Field::City, req.city, &revert_to_auto),
        country_code: suppress_if_reverted(Field::CountryCode, req.country_code, &revert_to_auto),
        country_name: suppress_if_reverted(Field::CountryName, req.country_name, &revert_to_auto),
        latitude: suppress_if_reverted(Field::Latitude, req.latitude, &revert_to_auto),
        longitude: suppress_if_reverted(Field::Longitude, req.longitude, &revert_to_auto),
        asn: suppress_if_reverted(Field::Asn, req.asn, &revert_to_auto),
        network_operator: suppress_if_reverted(
            Field::NetworkOperator,
            req.network_operator,
            &revert_to_auto,
        ),
        website: suppress_if_reverted(Field::Website, req.website, &revert_to_auto),
        notes: suppress_if_reverted(Field::Notes, req.notes, &revert_to_auto),
        revert_to_auto,
    };

    match repo::patch(&state.pool, id, patch_set).await {
        Ok(entry) => {
            state
                .catalogue_broker
                .publish(CatalogueEvent::Updated { id: entry.id });
            (StatusCode::OK, Json(CatalogueEntryDto::from(entry))).into_response()
        }
        Err(sqlx::Error::RowNotFound) => {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
        }
        Err(e) => db_error("catalogue patch: repo::patch failed", e),
    }
}

/// `DELETE /api/catalogue/{id}` — idempotent row removal.
///
/// Returns 204 whether or not the row existed: [`repo::delete`] issues a
/// plain `DELETE ... WHERE id = $1` and surfaces only the affected-row
/// count, so the HTTP surface always answers 204. The
/// [`CatalogueEvent::Deleted`] event is broadcast only when a row was
/// actually removed (`rows_affected > 0`) — redundant deletes against a
/// missing id complete silently to avoid waking SSE subscribers on a
/// no-op.
#[utoipa::path(
    delete,
    path = "/api/catalogue/{id}",
    tag = "catalogue",
    params(
        ("id" = Uuid, Path, description = "Catalogue row id"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn delete(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match repo::delete(&state.pool, id).await {
        Ok(rows_affected) => {
            if rows_affected > 0 {
                state
                    .catalogue_broker
                    .publish(CatalogueEvent::Deleted { id });
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => db_error("catalogue delete: repo::delete failed", e),
    }
}
