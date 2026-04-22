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

// Re-exported so callers keep importing `dto::Polygon` while the wire
// type's home module is `shapes` (which owns both the struct and the
// conversion into `geo::Polygon`).
pub use super::shapes::Polygon;

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
    /// Enrichment pipeline status. Ordered alphabetically by the text
    /// rendering of the enum (`enriched` < `failed` < `pending`), not
    /// by Postgres enum declaration order — the repo layer casts the
    /// column to `text` before comparing.
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
    /// Server-joined reverse-DNS hostname for [`Self::ip`].
    ///
    /// Populated by the handler after a single batched
    /// [`crate::hostname::hostnames_for`] lookup: `Some(_)` is a positive
    /// cache hit; `None` is either a confirmed-negative hit (serialized
    /// as absent via the skip-none attribute) or a cold miss. Cold
    /// misses also enqueue a background resolution scoped to the
    /// caller's session so the value arrives over the hostname SSE
    /// stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
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
            hostname: None,
        }
    }
}

/// Paste payload — a raw list of IP tokens. Each token is parsed by
/// [`super::parse::parse_ip_tokens`]; tokens may be bare IPs or host
/// CIDRs (`/32` for v4, `/128` for v6). Wider CIDRs and unparseable
/// tokens fall into [`PasteResponse::invalid`] instead of aborting the
/// whole request.
///
/// An optional [`PasteMetadata`] applies the same operator-set fields
/// to every accepted IP. Newly-created rows always receive the values
/// and have the corresponding field names appended to
/// `operator_edited_fields`. Existing rows receive a field only if it
/// is not already locked; paired fields (`Latitude`+`Longitude` and
/// `CountryCode`+`CountryName`) apply atomically — if either half of
/// a pair is locked, neither half is written. Requests without
/// `metadata` preserve the pre-T52 behaviour exactly (no writes to
/// `existing` rows, no `skipped_summary` on the response).
#[derive(Debug, Deserialize, ToSchema)]
pub struct PasteRequest {
    /// Raw tokens to parse and (when valid) insert into the catalogue.
    pub ips: Vec<String>,
    /// Optional default metadata applied to every accepted IP. See the
    /// struct-level docs for the merge semantics.
    #[serde(default)]
    pub metadata: Option<PasteMetadata>,
    /// Optional per-IP display-name overrides, keyed by the caller's
    /// literal IP string (same form as it appears in `ips`). An
    /// override supplies the display name for a single IP and wins
    /// over `metadata.display_name`; it still honours the lock rule on
    /// existing rows (an existing `DisplayName` lock blocks the
    /// override just like it blocks the panel default). Keys that do
    /// not parse as IPs, or that are not in the accepted set, are
    /// silently ignored — matches the permissive posture used by the
    /// keyset cursor and `ip_prefix` filter.
    #[serde(default)]
    pub per_ip_display_names: std::collections::HashMap<String, String>,
}

/// Operator-set default metadata applied to every accepted IP in a
/// paste.
///
/// Each field is independently optional — absent means "don't touch
/// this column". Two pairings are **atomic**: `latitude`+`longitude`
/// must be supplied together (or both omitted), and
/// `country_code`+`country_name` must be supplied together. The
/// paste handler rejects half-supplied pairs with 400
/// `paired_metadata_half_missing`.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct PasteMetadata {
    /// Operator-facing display label.
    #[serde(default)]
    pub display_name: Option<String>,
    /// City name.
    #[serde(default)]
    pub city: Option<String>,
    /// ISO 3166-1 alpha-2 country code. Must be paired with
    /// [`PasteMetadata::country_name`]. Validated as a 2-character
    /// ASCII-alphabetic string by the handler.
    #[serde(default)]
    pub country_code: Option<String>,
    /// Country human-readable name. Must be paired with
    /// [`PasteMetadata::country_code`].
    #[serde(default)]
    pub country_name: Option<String>,
    /// Decimal latitude in [-90, 90]. Must be paired with
    /// [`PasteMetadata::longitude`].
    #[serde(default)]
    pub latitude: Option<f64>,
    /// Decimal longitude in [-180, 180]. Must be paired with
    /// [`PasteMetadata::latitude`].
    #[serde(default)]
    pub longitude: Option<f64>,
    /// Operator-supplied external link.
    #[serde(default)]
    pub website: Option<String>,
    /// Free-form operator notes.
    #[serde(default)]
    pub notes: Option<String>,
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
/// paste outcome plus an optional aggregate describing metadata
/// writes that were skipped against existing rows.
#[derive(Debug, Serialize, ToSchema)]
pub struct PasteResponse {
    /// Rows newly inserted by this call.
    pub created: Vec<CatalogueEntryDto>,
    /// Rows already present in the catalogue. When the request
    /// carried a [`PasteMetadata`], each entry here reflects the
    /// post-merge row state — so the UI does not need a follow-up
    /// fetch to observe writes that survived the lock check.
    pub existing: Vec<CatalogueEntryDto>,
    /// Tokens rejected during parse.
    pub invalid: Vec<PasteInvalid>,
    /// Aggregate summary of metadata writes that were refused because
    /// the target field was already operator-locked. Absent when the
    /// request did not carry `metadata`; present (and possibly
    /// all-zero) otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_summary: Option<PasteSkippedSummary>,
}

/// Aggregate describing metadata writes the server refused to apply
/// because the target row already carried operator locks on the
/// affected fields.
///
/// Keys in [`PasteSkippedSummary::skipped_field_counts`] are canonical
/// [`super::model::Field::as_str`] names, plus two composite labels
/// for paired fields:
/// - `"Location"` — either half of `Latitude`/`Longitude` was locked.
/// - `"Country"`  — either half of `CountryCode`/`CountryName` was
///   locked.
///
/// The composite names let the UI narrate skips without reconstructing
/// the pairing client-side.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct PasteSkippedSummary {
    /// Count of rows that kept at least one operator-locked field.
    pub rows_with_skips: u32,
    /// Per-field skip counts; zero-count keys are omitted.
    pub skipped_field_counts: std::collections::BTreeMap<String, u32>,
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

/// Parse `minLat,minLon,maxLat,maxLon` into `[f64; 4]` — **required**.
///
/// Unlike [`deserialize_bbox`], a missing or malformed bbox surfaces as
/// a serde error (→ 400). The map endpoint always scopes to a viewport,
/// so there is no "silently drop the filter" fallback to lean on. See
/// [`MapQuery::bbox`] for the contract.
///
/// Additional validation beyond parse: every component must be finite
/// (no NaN / Infinity), latitudes in [-90, 90], longitudes in
/// [-180, 180], and `min_lat ≤ max_lat` / `min_lon ≤ max_lon`. Absent
/// these guards, callers panning past the antimeridian (Leaflet
/// `worldCopyJump`) or typoing the component order silently return
/// empty result sets because the SQL `BETWEEN` matches nothing —
/// which looks identical to "no rows in this viewport" to operators.
fn deserialize_bbox_required<'de, D>(de: D) -> Result<[f64; 4], D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(de)?;
    if raw.is_empty() {
        return Err(serde::de::Error::custom("bbox is required"));
    }
    let parts: Result<Vec<f64>, _> = raw
        .split(',')
        .map(str::trim)
        .map(str::parse::<f64>)
        .collect();
    let parts =
        parts.map_err(|e| serde::de::Error::custom(format!("invalid bbox component: {e}")))?;
    if parts.len() != 4 {
        return Err(serde::de::Error::custom(format!(
            "bbox must have exactly 4 components, got {}",
            parts.len()
        )));
    }
    let bbox = [parts[0], parts[1], parts[2], parts[3]];
    validate_bbox_geometry(&bbox).map_err(serde::de::Error::custom)?;
    Ok(bbox)
}

/// Enforce bbox invariants shared by every caller that supplies a
/// required bbox. Returns a human-readable error suitable for the
/// serde `custom` path. Extracted so the list endpoint can reuse the
/// same guard in a follow-up without duplicating the literals.
fn validate_bbox_geometry(bbox: &[f64; 4]) -> Result<(), String> {
    if !bbox.iter().all(|v| v.is_finite()) {
        return Err("bbox components must be finite".into());
    }
    let [min_lat, min_lon, max_lat, max_lon] = *bbox;
    if !(-90.0..=90.0).contains(&min_lat) || !(-90.0..=90.0).contains(&max_lat) {
        return Err(format!(
            "bbox latitudes must be in [-90, 90]; got min_lat={min_lat}, max_lat={max_lat}"
        ));
    }
    if !(-180.0..=180.0).contains(&min_lon) || !(-180.0..=180.0).contains(&max_lon) {
        return Err(format!(
            "bbox longitudes must be in [-180, 180]; got min_lon={min_lon}, max_lon={max_lon}"
        ));
    }
    if min_lat > max_lat {
        return Err(format!(
            "bbox min_lat ({min_lat}) exceeds max_lat ({max_lat})"
        ));
    }
    if min_lon > max_lon {
        return Err(format!(
            "bbox min_lon ({min_lon}) exceeds max_lon ({max_lon})"
        ));
    }
    Ok(())
}

/// Maximum zoom the map endpoint answers. Zooms above this all fall
/// into the finest cell band, so clamping here also normalises the
/// wire format and keeps `zoom: 255` from looking like an invariant
/// violation when `cell_size_for_zoom` returns a sane number anyway.
const MAP_ZOOM_MAX: u8 = 20;

/// Reject zoom values above [`MAP_ZOOM_MAX`] — the finest cluster cell
/// band already covers zoom 15+, so anything beyond 20 is either a
/// client bug or a probe. Surfaces as 400 rather than silently mapping
/// to the bottom band.
fn deserialize_zoom<'de, D>(de: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let z = u8::deserialize(de)?;
    if z > MAP_ZOOM_MAX {
        return Err(serde::de::Error::custom(format!(
            "zoom must be in 0..={MAP_ZOOM_MAX}; got {z}"
        )));
    }
    Ok(z)
}

/// Upper bound on the number of polygons a single request may carry.
/// Defence-in-depth: the rings feed an O(vertices × rows) point-in-
/// polygon scan, so combining many rings with many vertices is quadratic
/// on the happy path and linear in request size. Operators draw a
/// handful of AOIs at a time; 16 is well above observed UX need.
const MAX_SHAPES_PER_REQUEST: usize = 16;

/// Upper bound on vertices per polygon. See `MAX_SHAPES_PER_REQUEST`
/// for the shape-count counterpart; leaflet-geoman's draw tool caps
/// typical hand-drawn rings far below this, so this limit only fires
/// for programmatic callers or abuse.
const MAX_VERTICES_PER_SHAPE: usize = 1024;

/// Parse the `shapes` query parameter into `Vec<Polygon>`.
///
/// Accepts either an inline JSON array (`?shapes=[[[lng,lat],…]]`) or
/// an absent / empty value. An absent / empty value yields an empty
/// vec (no filter). Malformed JSON surfaces as a 400 via
/// [`serde::de::Error::custom`], matching the `asn` CSV-of-ints
/// behaviour: once the caller supplies a structurally-typed value, a
/// parse failure is surfaced rather than silently dropped.
///
/// Additional validation: rejects requests carrying more than
/// [`MAX_SHAPES_PER_REQUEST`] polygons or any single polygon over
/// [`MAX_VERTICES_PER_SHAPE`] vertices, and every polygon must pass
/// [`geo::Polygon::try_from`] (≥ 3 distinct vertices). Without the
/// try_from gate here, structurally-valid rings that can't form a
/// polygon (e.g. three colinear points, two-point rings) used to reach
/// the repo layer where they'd be silently dropped — producing empty
/// result pages whose `total` (SQL count) disagreed with the returned
/// `entries` (post-PIP filter).
fn deserialize_shapes_json<'de, D>(de: D) -> Result<Vec<Polygon>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    let Some(s) = raw else { return Ok(Vec::new()) };
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let polys: Vec<Polygon> = serde_json::from_str(&s).map_err(serde::de::Error::custom)?;
    if polys.len() > MAX_SHAPES_PER_REQUEST {
        return Err(serde::de::Error::custom(format!(
            "too many shapes: {} exceeds limit of {MAX_SHAPES_PER_REQUEST}",
            polys.len()
        )));
    }
    for (i, p) in polys.iter().enumerate() {
        if p.0.len() > MAX_VERTICES_PER_SHAPE {
            return Err(serde::de::Error::custom(format!(
                "shape {i}: {} vertices exceeds limit of {MAX_VERTICES_PER_SHAPE}",
                p.0.len()
            )));
        }
        if let Err(e) = geo::Polygon::<f64>::try_from(p) {
            return Err(serde::de::Error::custom(format!("shape {i}: {e}")));
        }
    }
    Ok(polys)
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

/// Filters for `GET /api/catalogue/map`.
///
/// Same filter set as [`ListQuery`] **minus** `sort` / `sort_dir` /
/// `after` / `shapes` / `city` — the map view is intentionally shape-
/// blind (operators still want to draw shapes against the unfiltered
/// geography of the fleet) and is not paginated. `bbox` is **required**
/// here — the map endpoint always scopes to a viewport. `zoom` drives
/// the grid cell size when the server falls back to cluster aggregation
/// (see [`MapResponse`]).
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MapQuery {
    /// Zero-or-more ISO 3166-1 alpha-2 codes. ANY semantics. CSV form
    /// (`?country_code=US,DE`) — see [`ListQuery::country_code`] for the
    /// rationale; repeat-key form is not accepted.
    #[serde(default, deserialize_with = "deserialize_csv_string")]
    #[param(style = Form, explode = false)]
    pub country_code: Vec<String>,
    /// Zero-or-more ASN numbers. ANY semantics. CSV form.
    #[serde(default, deserialize_with = "deserialize_csv_i32")]
    #[param(style = Form, explode = false)]
    pub asn: Vec<i32>,
    /// Zero-or-more `network_operator` ILIKE patterns. ANY semantics.
    /// CSV form.
    #[serde(default, deserialize_with = "deserialize_csv_string")]
    #[param(style = Form, explode = false)]
    pub network: Vec<String>,
    /// Optional IP prefix (CIDR or bare IP). Filters `c.ip <<= $prefix`;
    /// unparseable values are silently dropped (mirrors [`ListQuery`]).
    #[serde(default)]
    pub ip_prefix: Option<String>,
    /// Optional `display_name` substring. Same `%…%` wrap happens in
    /// the handler before it hits the repo.
    #[serde(default)]
    pub name: Option<String>,
    /// Viewport bounds — `minLat,minLon,maxLat,maxLon`. **Required.**
    /// A malformed or missing value surfaces as a 400 via serde, unlike
    /// [`ListQuery::bbox`] which permissively drops the filter.
    #[serde(deserialize_with = "deserialize_bbox_required")]
    #[param(style = Form, explode = false)]
    pub bbox: [f64; 4],
    /// Zoom level (0..=20). Controls the grid cell size for
    /// cluster aggregation when the filtered count crosses
    /// [`super::repo::MAP_DETAIL_THRESHOLD`]. See
    /// [`super::repo::cell_size_for_zoom`] for the mapping.
    /// Values above 20 surface as 400 — leaflet doesn't advertise
    /// zooms beyond that on any of our tile backends.
    #[serde(deserialize_with = "deserialize_zoom")]
    pub zoom: u8,
}

/// Adaptive response for `GET /api/catalogue/map`.
///
/// - `detail` — raw rows — when the filtered count in the request bbox
///   is at or below [`super::repo::MAP_DETAIL_THRESHOLD`].
/// - `clusters` — grid-aggregated buckets — when above the threshold.
///
/// The wire form carries a `kind` discriminator (`"detail"` /
/// `"clusters"`) so clients can branch without inspecting the variant's
/// fields. See [`super::repo::map_detail_or_clusters`] for the
/// server-side selection logic.
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MapResponse {
    /// Raw rows — returned when the filtered count stays at or below
    /// the detail threshold.
    Detail {
        /// Matching rows, ordered by `(created_at DESC, id DESC)`.
        rows: Vec<CatalogueEntryDto>,
        /// Count of all rows matching the filter within the viewport.
        total: i64,
    },
    /// Grid-aggregated buckets — returned when the filtered count
    /// exceeds the detail threshold.
    Clusters {
        /// One bucket per occupied grid cell.
        buckets: Vec<MapBucket>,
        /// Total row count in the viewport before aggregation.
        total: i64,
        /// Grid cell size in degrees; matches
        /// [`super::repo::cell_size_for_zoom`] for the request's `zoom`.
        cell_size: f64,
    },
}

/// One grid-aggregated cluster cell surfaced by [`MapResponse::Clusters`].
#[derive(Debug, Serialize, ToSchema)]
pub struct MapBucket {
    /// Cell center latitude.
    pub lat: f64,
    /// Cell center longitude.
    pub lng: f64,
    /// Rows inside the cell.
    pub count: i64,
    /// A deterministically-chosen row id from inside the cell — useful
    /// for client-side drill-down without re-querying the whole cell.
    pub sample_id: Uuid,
    /// Cell bounding box as `[min_lat, min_lng, max_lat, max_lng]`.
    pub bbox: [f64; 4],
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

#[cfg(test)]
mod tests {
    //! Focused coverage for the custom query deserializers that guard
    //! the `shapes` wire surface and the map endpoint's `bbox` / `zoom`.
    //! Each function carries its own adversarial inputs so a regression
    //! in one validator can't silently leak through another. Tests use
    //! `serde_json::from_value` because it matches the deserializers'
    //! `Option<String>` / `String` / `u8` input shape without pulling
    //! in an extra url-encoding dev-dep.
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    struct ShapesEnvelope {
        #[serde(default, deserialize_with = "deserialize_shapes_json")]
        shapes: Vec<Polygon>,
    }

    fn parse_shapes(raw: &str) -> Result<Vec<Polygon>, String> {
        serde_json::from_value::<ShapesEnvelope>(json!({ "shapes": raw }))
            .map(|e| e.shapes)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn shapes_rejects_below_three_distinct_vertices() {
        // Two-point ring. Previously silently dropped in repo::list,
        // producing empty entries with non-zero total.
        let err = parse_shapes("[[[0,0],[1,1]]]").unwrap_err();
        assert!(err.contains("shape 0"), "unexpected error: {err}");
    }

    #[test]
    fn shapes_rejects_excessive_polygon_count() {
        let one = "[[0,0],[1,0],[1,1],[0,0]]";
        let many: Vec<&str> = std::iter::repeat_n(one, MAX_SHAPES_PER_REQUEST + 1).collect();
        let raw = format!("[{}]", many.join(","));
        let err = parse_shapes(&raw).unwrap_err();
        assert!(err.contains("too many shapes"), "unexpected error: {err}");
    }

    #[test]
    fn shapes_rejects_excessive_vertex_count() {
        let mut verts = String::from("[");
        for i in 0..(MAX_VERTICES_PER_SHAPE + 2) {
            let x = (i as f64) * 0.001;
            verts.push_str(&format!("[{x},0],"));
        }
        verts.push_str("[0,0]]");
        let raw = format!("[{verts}]");
        let err = parse_shapes(&raw).unwrap_err();
        assert!(err.contains("exceeds limit"), "unexpected error: {err}");
    }

    #[test]
    fn shapes_accepts_well_formed_polygon() {
        let polys = parse_shapes("[[[0,0],[1,0],[1,1],[0,1],[0,0]]]").unwrap();
        assert_eq!(polys.len(), 1);
        assert_eq!(polys[0].0.len(), 5);
    }

    #[derive(Debug, Deserialize)]
    struct BboxEnvelope {
        #[serde(deserialize_with = "deserialize_bbox_required")]
        bbox: [f64; 4],
    }

    fn parse_bbox(raw: &str) -> Result<[f64; 4], String> {
        serde_json::from_value::<BboxEnvelope>(json!({ "bbox": raw }))
            .map(|e| e.bbox)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn bbox_rejects_nan() {
        let err = parse_bbox("NaN,0,10,10").unwrap_err();
        assert!(err.contains("finite"), "unexpected error: {err}");
    }

    #[test]
    fn bbox_rejects_out_of_range_latitude() {
        let err = parse_bbox("-100,0,100,10").unwrap_err();
        assert!(err.contains("latitudes"), "unexpected error: {err}");
    }

    #[test]
    fn bbox_rejects_out_of_range_longitude() {
        // Mirrors what `worldCopyJump` produces when the operator pans
        // past the antimeridian — before this guard the server silently
        // returned an empty page, now the client gets a 400 it can
        // clamp against.
        let err = parse_bbox("-10,-200,10,200").unwrap_err();
        assert!(err.contains("longitudes"), "unexpected error: {err}");
    }

    #[test]
    fn bbox_rejects_inverted_axes() {
        let err_lat = parse_bbox("30,0,10,10").unwrap_err();
        assert!(err_lat.contains("min_lat"), "unexpected error: {err_lat}");
        let err_lon = parse_bbox("0,30,10,10").unwrap_err();
        assert!(err_lon.contains("min_lon"), "unexpected error: {err_lon}");
    }

    #[test]
    fn bbox_accepts_canonical_viewport() {
        let bbox = parse_bbox("-10,-20,10,20").unwrap();
        assert_eq!(bbox, [-10.0, -20.0, 10.0, 20.0]);
    }

    #[derive(Debug, Deserialize)]
    struct ZoomEnvelope {
        #[serde(deserialize_with = "deserialize_zoom")]
        zoom: u8,
    }

    #[test]
    fn zoom_rejects_above_max() {
        let err = serde_json::from_value::<ZoomEnvelope>(json!({ "zoom": 21 })).unwrap_err();
        assert!(err.to_string().contains("0..=20"));
    }

    #[test]
    fn zoom_accepts_within_range() {
        let z = serde_json::from_value::<ZoomEnvelope>(json!({ "zoom": 12 })).unwrap();
        assert_eq!(z.zoom, 12);
    }
}
