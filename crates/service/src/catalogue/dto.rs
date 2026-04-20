//! Wire shapes for the operator HTTP catalogue surface.
//!
//! Why a dedicated module:
//! - `CatalogueEntry` (see [`super::model::CatalogueEntry`]) is the
//!   in-process row shape — it uses `IpAddr`, which serializes via
//!   `serde` in a shape the frontend doesn't want (an arrays/ints blob).
//!   [`CatalogueEntryDto`] flattens `ip` to a display-style string and
//!   drops internal bookkeeping we don't want to expose.
//! - [`PasteRequest`] / [`PasteResponse`] / [`PasteInvalid`] express
//!   the paste endpoint's mixed-outcome body: accepted IPs are split
//!   into newly-created vs already-existing, rejected tokens carry
//!   per-token rejection reasons.
//! - [`ListQuery`] drives the filter surface on `GET /api/catalogue`.
//!   It's an [`utoipa::IntoParams`] so the OpenAPI schema advertises
//!   every filter key (`?country_code=…&asn=…&network=…&ip_prefix=…`).
//! - [`PatchRequest`] is declared here for T12 so the DTO layout lives
//!   in one file and the enclosing module doesn't have to grow another
//!   `patch.rs` in a later task.

use super::model::{CatalogueEntry, CatalogueSource, EnrichmentStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Sort columns accepted by `GET /api/catalogue`.
///
/// The list handler always appends `id DESC` as the tiebreaker so the
/// ordering is total even when multiple rows share the same sort
/// value, and every variant treats nullable columns as `NULLS LAST`
/// regardless of direction. Both invariants are load-bearing for the
/// keyset cursor — see [`super::sort::Cursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    /// Row creation timestamp — the default.
    #[default]
    CreatedAt,
    /// Catalogued IP address (text form for ordering).
    Ip,
    /// Operator-supplied display label.
    DisplayName,
    /// City name.
    City,
    /// ISO 3166-1 alpha-2 country code.
    CountryCode,
    /// Autonomous system number.
    Asn,
    /// Network operator / ISP name.
    NetworkOperator,
    /// Enrichment pipeline status.
    EnrichmentStatus,
    /// Operator-supplied external link.
    Website,
    /// Derived "row has coordinates" boolean — rows with lat+lng land
    /// before rows without, regardless of direction; `NULLS LAST` applies.
    Location,
}

/// Sort direction for [`SortBy`]. `NULLS LAST` applies to both arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    /// Ascending — smallest first, NULLs at the tail.
    Asc,
    /// Descending — largest first, NULLs at the tail.
    #[default]
    Desc,
}

/// GeoJSON-compatible polygon ring expressed as `[lng, lat]` pairs.
///
/// The ring is implicitly closed — the server appends the closing
/// vertex if absent. A minimum of three distinct points is required;
/// the wire-type conversion in [`super::shapes`] (added in Task 2)
/// rejects shorter rings.
///
/// The `[lng, lat]` order matches GeoJSON and `@turf/helpers`, so the
/// frontend can feed Turf outputs straight through without reordering.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct Polygon(pub Vec<[f64; 2]>);

/// Operator-facing view of a single catalogue row.
///
/// This is the wire shape returned by `GET /api/catalogue/{id}` and
/// embedded in [`ListResponse::entries`] / [`PasteResponse::created`].
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CatalogueEntryDto {
    /// Primary key (UUID v4).
    pub id: Uuid,
    /// Catalogued IP, rendered via `IpAddr::to_string()` (no CIDR
    /// prefix — the row is always a host address).
    pub ip: String,
    /// Operator-supplied display label. Absent when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// City name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// ISO 3166-1 alpha-2 country code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    /// Country human-readable name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_name: Option<String>,
    /// Decimal latitude.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    /// Decimal longitude.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// Autonomous system number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asn: Option<i32>,
    /// Network operator / ISP name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_operator: Option<String>,
    /// Operator-supplied external link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    /// Free-form operator notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Current enrichment pipeline status.
    pub enrichment_status: EnrichmentStatus,
    /// Timestamp of the most recent successful enrichment run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enriched_at: Option<DateTime<Utc>>,
    /// Columns the operator has explicitly edited. PascalCase strings
    /// matching [`super::model::Field::as_str`].
    pub operator_edited_fields: Vec<String>,
    /// Where the row originated.
    pub source: CatalogueSource,
    /// Row creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Operator principal (session username) that created the row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

impl From<CatalogueEntry> for CatalogueEntryDto {
    fn from(e: CatalogueEntry) -> Self {
        Self {
            id: e.id,
            ip: e.ip.to_string(),
            display_name: e.display_name,
            city: e.city,
            country_code: e.country_code,
            country_name: e.country_name,
            latitude: e.latitude,
            longitude: e.longitude,
            asn: e.asn,
            network_operator: e.network_operator,
            website: e.website,
            notes: e.notes,
            enrichment_status: e.enrichment_status,
            enriched_at: e.enriched_at,
            operator_edited_fields: e.operator_edited_fields,
            source: e.source,
            created_at: e.created_at,
            created_by: e.created_by,
        }
    }
}

/// Paste payload — a raw list of IP tokens. Each token is parsed by
/// [`super::parse::parse_ip_tokens`]; tokens may be bare IPs or host
/// CIDRs (`/32` for v4, `/128` for v6). Wider CIDRs and unparseable
/// tokens fall into [`PasteResponse::invalid`] instead of aborting the
/// whole request.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PasteRequest {
    /// Raw tokens to parse and (when valid) insert into the catalogue.
    pub ips: Vec<String>,
}

/// Per-token rejection surfaced by [`PasteResponse::invalid`].
#[derive(Debug, Serialize, ToSchema)]
pub struct PasteInvalid {
    /// The exact token as received from the client.
    pub token: String,
    /// Short human-readable reason — intended for immediate UI display.
    pub reason: String,
}

/// Response body for `POST /api/catalogue` — a three-way split of the
/// paste outcome.
#[derive(Debug, Serialize, ToSchema)]
pub struct PasteResponse {
    /// Rows newly inserted by this call.
    pub created: Vec<CatalogueEntryDto>,
    /// Rows already present in the catalogue. Surfaces the existing
    /// enrichment state without a follow-up fetch.
    pub existing: Vec<CatalogueEntryDto>,
    /// Tokens rejected during parse.
    pub invalid: Vec<PasteInvalid>,
}

/// Query-string filter set accepted by `GET /api/catalogue`.
///
/// Multi-valued fields (`country_code`, `asn`, `network`) accept
/// comma-separated lists (`?country_code=US,DE`). The axum default
/// `Query` extractor uses `serde_urlencoded`, which does not support
/// repeated keys for `Vec<T>` — the CSV form is the supported on-wire
/// syntax. `bbox` accepts `minLat,minLon,maxLat,maxLon`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    /// Zero-or-more ISO 3166-1 alpha-2 codes. ANY semantics.
    ///
    /// CSV string; e.g. `?country_code=US,DE`. Repeat-key form
    /// (`?country_code=US&country_code=DE`) is NOT supported — axum's
    /// default `Query` extractor is `serde_urlencoded`, which does not
    /// deserialize repeated keys into `Vec<T>`.
    #[serde(default, deserialize_with = "deserialize_csv_string")]
    #[param(style = Form, explode = false)]
    pub country_code: Vec<String>,
    /// Zero-or-more ASN numbers. ANY semantics.
    ///
    /// CSV string; e.g. `?asn=64500,64501`. See `country_code` for
    /// rationale; repeat-key form is not accepted.
    #[serde(default, deserialize_with = "deserialize_csv_i32")]
    #[param(style = Form, explode = false)]
    pub asn: Vec<i32>,
    /// Zero-or-more `network_operator` ILIKE patterns. ANY semantics.
    /// Wildcards are the caller's responsibility.
    ///
    /// CSV string; e.g. `?network=foo,bar`. See `country_code` for
    /// rationale; repeat-key form is not accepted.
    #[serde(default, deserialize_with = "deserialize_csv_string")]
    #[param(style = Form, explode = false)]
    pub network: Vec<String>,
    /// Optional IP prefix (CIDR or bare IP). Filters `c.ip <<= $prefix`
    /// (contained-or-equal) when parseable so bare-host queries match
    /// their own `/32` / `/128` row as well as CIDR prefixes; an
    /// unparseable value is silently dropped.
    #[serde(default)]
    pub ip_prefix: Option<String>,
    /// Optional `display_name` substring. Passed verbatim to the
    /// handler, which wraps it with `%…%` before running an `ILIKE`
    /// match, so callers send the literal substring they want to find
    /// (e.g. `?name=Fastly`). `%` / `_` characters in the input are
    /// intentionally not escaped — they behave as ILIKE wildcards.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional bounding box as a CSV string; exactly four floats
    /// `minLat,minLon,maxLat,maxLon`. Permissive parse — malformed
    /// values silently yield no filter, matching `ip_prefix` semantics.
    #[serde(default, deserialize_with = "deserialize_bbox")]
    #[param(style = Form, explode = false)]
    pub bbox: Option<[f64; 4]>,
    /// Zero-or-more city names. ANY semantics; exact match.
    ///
    /// CSV string; e.g. `?city=Berlin,Paris`. See `country_code` for
    /// rationale; repeat-key form is not accepted.
    #[serde(default, deserialize_with = "deserialize_csv_string")]
    #[param(style = Form, explode = false)]
    pub city: Vec<String>,
    /// Zero-or-more polygon shapes (point-in-any OR semantics).
    ///
    /// Accepts an inline JSON array of `[[lng, lat], ...]` rings, URL-
    /// encoded into the query string. The server computes the union
    /// bbox as a cheap SQL pre-filter and then runs exact point-in-
    /// polygon over the returned page — see [`super::shapes`] (Task 2).
    ///
    /// Malformed JSON is rejected with a 400 via
    /// [`serde::de::Error::custom`], matching the `asn` CSV-of-ints
    /// behaviour: filter *values* may be advisory elsewhere, but once a
    /// value is present and structurally typed, a parse failure is a
    /// caller bug we surface rather than silently drop.
    #[serde(default, deserialize_with = "deserialize_shapes_json")]
    #[param(value_type = String)]
    pub shapes: Vec<Polygon>,
    /// Sort column — defaults to [`SortBy::CreatedAt`]. `NULLS LAST`
    /// applies; `id DESC` is the invariant tiebreaker.
    #[serde(default)]
    pub sort: SortBy,
    /// Sort direction — defaults to [`SortDir::Desc`].
    #[serde(default)]
    pub sort_dir: SortDir,
    /// Opaque keyset cursor returned by a prior call's `next_cursor`.
    /// Absent for the first page. See [`super::sort::Cursor`] for the
    /// wire format and the server-side revalidation rules.
    #[serde(default)]
    pub after: Option<String>,
    /// Page size. Clamped to `1..=500` internally; default 100.
    #[serde(default = "default_limit")]
    pub limit: i64,
}

/// Default page size for [`ListQuery::limit`].
fn default_limit() -> i64 {
    100
}

/// Parse a comma-separated string into `Vec<String>`. Absent → empty
/// vec; empty string → empty vec; trims each token.
fn deserialize_csv_string<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    Ok(raw
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default())
}

/// Parse a comma-separated string into `Vec<i32>`. Unparseable tokens
/// surface as deserialization errors so the caller sees a 400 rather
/// than silently-dropped filters.
fn deserialize_csv_i32<'de, D>(de: D) -> Result<Vec<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    let Some(s) = raw else { return Ok(Vec::new()) };
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.parse::<i32>().map_err(serde::de::Error::custom))
        .collect()
}

/// Parse `minLat,minLon,maxLat,maxLon` into `[f64; 4]`. Permissive
/// parse — any malformed value (non-numeric tokens, wrong arity,
/// empty string, absent key) silently yields `Ok(None)` so no bbox
/// filter is applied. This matches the `ip_prefix` silent-drop
/// semantics: filter inputs are advisory and the caller gets a
/// successful response with no filter rather than a 400.
fn deserialize_bbox<'de, D>(de: D) -> Result<Option<[f64; 4]>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    let Some(s) = raw else { return Ok(None) };
    if s.is_empty() {
        return Ok(None);
    }
    let parts: Result<Vec<f64>, _> = s.split(',').map(str::trim).map(str::parse::<f64>).collect();
    let Ok(parts) = parts else { return Ok(None) };
    if parts.len() != 4 {
        return Ok(None);
    }
    Ok(Some([parts[0], parts[1], parts[2], parts[3]]))
}

/// Parse the `shapes` query parameter into `Vec<Polygon>`.
///
/// Accepts either an inline JSON array (`?shapes=[[[lng,lat],…]]`) or
/// an absent / empty value. An absent / empty value yields an empty
/// vec (no filter). Malformed JSON surfaces as a 400 via
/// [`serde::de::Error::custom`], matching the `asn` CSV-of-ints
/// behaviour: once the caller supplies a structurally-typed value, a
/// parse failure is surfaced rather than silently dropped.
fn deserialize_shapes_json<'de, D>(de: D) -> Result<Vec<Polygon>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    let Some(s) = raw else { return Ok(Vec::new()) };
    if s.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<Polygon>>(&s).map_err(serde::de::Error::custom)
}

/// Response body for `GET /api/catalogue`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ListResponse {
    /// Matching rows ordered by the request's `sort` / `sort_dir` with
    /// `id DESC` as the tiebreaker; nullable sort columns place NULLs
    /// at the tail regardless of direction.
    pub entries: Vec<CatalogueEntryDto>,
    /// Count of all rows matching the filter (ignores `limit`).
    ///
    /// When the `shapes` filter is non-empty, `total` is an upper-
    /// bound approximation: SQL can only pre-filter by the shapes'
    /// union bounding box, so rows inside the bbox but outside every
    /// polygon are counted here while the corresponding point-in-
    /// polygon pass drops them from `entries`. Clients that need an
    /// exact post-shape count must sum `entries.len()` across every
    /// page.
    pub total: i64,
    /// Forward-paging token. `Some` when the server filled `limit`
    /// rows and a subsequent page may exist; `None` when the end of
    /// the result set has been reached. See [`super::sort::Cursor`]
    /// for the wire format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Error envelope used by every non-2xx catalogue response.
///
/// The single `error` field carries a stable, machine-parseable
/// snake_case code (e.g. `not_found`, `database_error`). Matches the
/// gateway-level JSON 404 emitted by `crate::http::backend_path_404`
/// so clients can use one shape for every `/api` error.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorEnvelope {
    /// Stable error code. Clients should match on this string, not on
    /// the HTTP status alone.
    pub error: String,
}

/// PATCH payload for `PATCH /api/catalogue/{id}` (declared here for T12
/// so all catalogue wire shapes live in one module).
///
/// Triple-state field encoding (outer `Option` = touched?, inner
/// `Option` = NULL?) mirrors [`super::repo::PatchValue`]. Callers omit
/// the JSON key for "leave untouched", send `null` for "set NULL",
/// and send a concrete value for "set to this".
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct PatchRequest {
    /// New display name. See the struct doc for triple-state encoding.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub display_name: Option<Option<String>>,
    /// New city.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub city: Option<Option<String>>,
    /// New ISO 3166-1 alpha-2 country code.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub country_code: Option<Option<String>>,
    /// New country human-readable name.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub country_name: Option<Option<String>>,
    /// New latitude.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub latitude: Option<Option<f64>>,
    /// New longitude.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub longitude: Option<Option<f64>>,
    /// New ASN.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub asn: Option<Option<i32>>,
    /// New network operator.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub network_operator: Option<Option<String>>,
    /// New website URL.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub website: Option<Option<String>>,
    /// New notes.
    #[serde(default, deserialize_with = "deserialize_triple_state")]
    pub notes: Option<Option<String>>,
    /// Names of fields the operator wants reverted to automatic
    /// enrichment. Values must match [`super::model::Field::as_str`].
    #[serde(default)]
    pub revert_to_auto: Vec<String>,
}

/// Request body for `POST /api/catalogue/reenrich`.
///
/// Best-effort bulk enqueue: each id is pushed onto the enrichment
/// queue without a prior existence check. Unknown ids resolve to a
/// no-op inside the runner (the row lookup simply returns none), so
/// callers may include speculative ids without surfacing a per-id
/// error path.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkReenrichRequest {
    /// Catalogue row ids the operator wants to re-run through the
    /// enrichment pipeline.
    pub ids: Vec<Uuid>,
}

/// Deserialize a triple-state field: absent → `None`,
/// `null` → `Some(None)`, value → `Some(Some(v))`.
///
/// Used by [`PatchRequest`] so serde can distinguish "leave unchanged"
/// from "clear column to NULL".
fn deserialize_triple_state<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}
