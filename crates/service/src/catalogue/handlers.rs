//! HTTP handlers for the operator-facing catalogue surface.
//!
//! - `POST /api/catalogue` — paste-and-insert. Parses tokens, inserts
//!   the accepted IPs, splits the result into created / existing /
//!   invalid buckets and enqueues each newly-created id for enrichment.
//! - `GET /api/catalogue` — filtered list. Multi-valued filters use ANY
//!   semantics (see [`super::dto::ListQuery`]); `sort` / `sort_dir` /
//!   `after` / `city` / `shapes` drive the keyset-paginated query in
//!   [`super::repo::list`]. The handler decodes the opaque `after`
//!   cursor via [`super::sort::Cursor::decode`]; a decode failure is
//!   treated as "no cursor" and the handler serves the first page —
//!   same posture as a sort-mismatched cursor (see repo-level docs).
//! - `GET /api/catalogue/{id}` — single-row fetch.
//! - `PATCH /api/catalogue/{id}` — partial update with revert-to-auto
//!   support. Touched fields flip into `operator_edited_fields`; names
//!   listed in `revert_to_auto` are NULLed and removed from the lock
//!   set so the next enrichment run re-populates them.
//! - `DELETE /api/catalogue/{id}` — idempotent removal.
//! - `POST /api/catalogue/{id}/reenrich` — enqueue a single row for a
//!   fresh enrichment pass.
//! - `POST /api/catalogue/reenrich` — bulk re-enrichment of the given
//!   id list (best-effort; unknown ids are silently dropped by the
//!   runner).
//! - `GET /api/catalogue/facets` — cached facet buckets driving the
//!   filter UI.
//!
//! Every handler lives behind the `login_required!` gate via
//! [`crate::http::openapi::api_router`] wiring; anonymous callers
//! short-circuit with 401 before the handler runs. The SSE route +
//! OpenAPI registration consolidate in T16.

use super::dto::{
    BulkReenrichRequest, CatalogueEntryDto, ErrorEnvelope, ListQuery, ListResponse, PasteInvalid,
    PasteRequest, PasteResponse, PatchRequest,
};
use super::events::CatalogueEvent;
use super::model::{CatalogueSource, Field};
use super::parse::{parse_ip_tokens, ParseReason};
use super::repo;
use super::repo::FacetsResponse;
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

    // New rows shift the facet bucket counts — invalidate the cache so
    // the next GET /api/catalogue/facets reflects the paste immediately
    // rather than waiting up to 30 s for the TTL to expire.
    if !outcome.created.is_empty() {
        state.facets_cache.invalidate().await;
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

/// `GET /api/catalogue` — filtered, size-bounded, keyset-paginated list.
///
/// The handler converts [`ListQuery`] straight into [`repo::ListFilter`]
/// and returns a [`ListResponse`]. The repo layer orders by the selected
/// `(sort, sort_dir)` with `id DESC` as the invariant tiebreaker and
/// `NULLS LAST` for every column; `shapes` run a cheap bbox pre-filter
/// in SQL plus exact point-in-polygon in Rust over the returned page
/// (see [`super::repo::list`] for the `total`-is-approximate caveat).
///
/// The wire `after` string is decoded via [`super::sort::Cursor::decode`].
/// A decode error (malformed base64 or JSON) silently degrades to "no
/// cursor" — the handler serves the first page rather than returning a
/// 400. The repo additionally discards a decoded cursor whose
/// `(sort, dir)` disagrees with the request's `(sort, dir)`, so a sort
/// change on the client naturally invalidates stale cursors.
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
    // The `name` query param is a user-facing substring filter. The repo
    // binds it verbatim into `display_name ILIKE $5`, so wrap the value
    // here so callers can pass `?name=Fastly` and match rows whose
    // `display_name` *contains* "Fastly" rather than having to send
    // the raw `%Fastly%` themselves. ILIKE treats `%` / `_` as wildcards
    // and the user-supplied characters pass through unescaped — that
    // matches other catalogue search surfaces (e.g. `agents.name`).
    let name_filter = q
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"));

    // Decode the opaque cursor. Decode failures (malformed base64 or
    // JSON) silently degrade to "no cursor" — the repo then serves the
    // first page. This matches the wire-contract note in the
    // `super::sort` module-level docs and keeps the client-side state
    // machine simple.
    let after = q
        .after
        .as_deref()
        .and_then(|raw| super::sort::Cursor::decode(raw).ok());

    let filter = repo::ListFilter {
        country_code: q.country_code,
        asn: q.asn,
        network: q.network,
        ip_prefix: q.ip_prefix,
        name: name_filter,
        bounding_box: q.bbox,
        city: q.city,
        shapes: q.shapes,
        sort: q.sort,
        sort_dir: q.sort_dir,
        after,
        limit: q.limit,
    };
    match repo::list(&state.pool, filter).await {
        Ok((entries, total, next_cursor)) => {
            let body = ListResponse {
                entries: entries.into_iter().map(CatalogueEntryDto::from).collect(),
                total,
                next_cursor: next_cursor.map(|c| c.encode()),
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
        (status = 400, description = "Invalid payload", body = ErrorEnvelope),
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
    // Input validation — reject before we touch the DB so obvious
    // client mistakes come back as 400 instead of either persisting
    // garbage or leaking a Postgres type/length error as a 500.
    //
    // Only validate values the caller is *setting*
    // (`Some(Some(_))` — outer Some = touched, inner Some = value).
    // `Some(None)` (explicit NULL) and `None` (untouched) are always
    // fine and pass through unchecked.
    if let Some(Some(lat)) = req.latitude {
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_latitude" })),
            )
                .into_response();
        }
    }
    if let Some(Some(lon)) = req.longitude {
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_longitude" })),
            )
                .into_response();
        }
    }
    if let Some(Some(code)) = req.country_code.as_ref() {
        // `country_code` is `CHAR(2)` — a wrong length would otherwise
        // surface as a Postgres error routed through `db_error` (500).
        // Accept only ASCII alphabetic 2-character codes (upper- or
        // lower-case) so the DB column stays well-formed.
        if code.len() != 2 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_country_code" })),
            )
                .into_response();
        }
    }

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
            // Any PATCH may change country_code, asn, or network_operator —
            // the fields that drive facet buckets. Invalidate unconditionally
            // rather than inspecting which fields were actually touched; the
            // cost of an extra DB round-trip is lower than missing a bucket
            // change because a field-level introspection was too conservative.
            state.facets_cache.invalidate().await;
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
                // The deleted row leaves its facet buckets — invalidate so
                // the next facets GET reflects the removal immediately.
                state.facets_cache.invalidate().await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => db_error("catalogue delete: repo::delete failed", e),
    }
}

/// `POST /api/catalogue/{id}/reenrich` — enqueue a single row for a
/// fresh enrichment pass.
///
/// Returns 202 Accepted when the id exists (the enrichment runner will
/// pick the row up asynchronously) and 404 when the id is unknown. The
/// existence check runs synchronously because a 404 is cheap to surface
/// and spares callers from an "accepted then silently dropped" UX.
///
/// Before enqueuing, the row is flipped back to `enrichment_status =
/// 'pending'` via [`repo::mark_enrichment_start`]. This makes the sweep
/// a true safety net: if the bounded queue is full (or its receiver is
/// gone) and the enqueue drops, the row is already `pending` with an
/// older `created_at`, so the runner's 30-second sweep will pick it up
/// on the next tick instead of leaving the re-enrich request silently
/// stranded (sweep only scans rows in `pending`, so a still-`enriched`
/// row would otherwise never be retried).
#[utoipa::path(
    post,
    path = "/api/catalogue/{id}/reenrich",
    tag = "catalogue",
    params(
        ("id" = Uuid, Path, description = "Catalogue row id"),
    ),
    responses(
        (status = 202, description = "Re-enrichment enqueued"),
        (status = 401, description = "No active session"),
        (status = 404, description = "Row not found", body = ErrorEnvelope),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn reenrich_one(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match repo::find_by_id(&state.pool, id).await {
        Ok(Some(_)) => {
            if let Err(e) = repo::mark_enrichment_start(&state.pool, id).await {
                return db_error("catalogue reenrich_one: mark_enrichment_start failed", e);
            }
            // If the row is deleted between the mark and the enqueue, the
            // enrichment runner handles the now-missing row gracefully —
            // benign race.
            let _ = state.enrichment_queue.enqueue(id);
            // Flipping a row back to `pending` shifts the enrichment_status
            // facet bucket — invalidate so the filter rail reflects the
            // change without waiting for the TTL.
            state.facets_cache.invalidate().await;
            StatusCode::ACCEPTED.into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response(),
        Err(e) => db_error("catalogue reenrich_one: find_by_id failed", e),
    }
}

/// Maximum `ids.len()` accepted by [`reenrich_many`].
///
/// Mirrors the clamp policy applied to the other catalogue endpoints
/// (`list.limit` → 500, facets → 250 per category) so a malformed or
/// malicious client cannot make one request walk O(N) database and
/// queue-send work.
pub const MAX_BULK_REENRICH_IDS: usize = 512;

/// `POST /api/catalogue/reenrich` — bulk re-enrichment.
///
/// Each id in [`BulkReenrichRequest::ids`] is flipped back to
/// `enrichment_status = 'pending'` in a single
/// [`repo::mark_enrichment_start_bulk`] call and then pushed onto the
/// enrichment queue. Unknown ids silently no-op at both layers (the
/// bulk UPDATE matches zero rows and the runner tolerates missing ids
/// on dequeue), so callers may include speculative ids without
/// surfacing a per-id error path.
///
/// The prior mark-`pending` step makes the sweep a true safety net:
/// queue drops (backpressure or closed channel) no longer silently lose
/// re-enrich requests because the row is already `pending` with an
/// older `created_at`, so the runner's 30-second sweep will pick it up
/// on the next tick.
///
/// Returns 400 when `ids.len() > MAX_BULK_REENRICH_IDS` and 202 on
/// success (including the empty-`ids` case).
#[utoipa::path(
    post,
    path = "/api/catalogue/reenrich",
    tag = "catalogue",
    request_body = BulkReenrichRequest,
    responses(
        (status = 202, description = "Bulk re-enrichment enqueued"),
        (status = 400, description = "Too many ids", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn reenrich_many(
    State(state): State<AppState>,
    Json(body): Json<BulkReenrichRequest>,
) -> Response {
    if body.ids.len() > MAX_BULK_REENRICH_IDS {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "too_many_ids" })),
        )
            .into_response();
    }
    if body.ids.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    if let Err(e) = repo::mark_enrichment_start_bulk(&state.pool, &body.ids).await {
        return db_error(
            "catalogue reenrich_many: mark_enrichment_start_bulk failed",
            e,
        );
    }
    for id in body.ids {
        let _ = state.enrichment_queue.enqueue(id);
    }
    // Bulk flip shifts the enrichment_status facet bucket for every
    // matched row — invalidate once after the full write completes.
    state.facets_cache.invalidate().await;
    StatusCode::ACCEPTED.into_response()
}

/// `GET /api/catalogue/facets` — cached aggregate facets for the
/// filter UI.
///
/// Serves [`FacetsResponse`] from the TTL cache on
/// [`crate::state::AppState::facets_cache`] so the four-group-by
/// aggregation runs at most once per cache window per process. The
/// cache refreshes lazily on access; a DB error during refresh is
/// surfaced as 500 without polluting the cached value.
#[utoipa::path(
    get,
    path = "/api/catalogue/facets",
    tag = "catalogue",
    responses(
        (status = 200, description = "Facet buckets", body = FacetsResponse),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn facets(State(state): State<AppState>) -> Response {
    match state.facets_cache.get(&state.pool).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => db_error("catalogue facets: FacetsCache::get failed", e),
    }
}
