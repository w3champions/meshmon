//! sqlx-backed CRUD for `ip_catalogue`.
//!
//! Every write path that carries operator intent (UI PATCH, agent Register)
//! extends `operator_edited_fields` with the canonical
//! [`super::model::Field::as_str`] name. The enrichment runner
//! (added in later tasks) must consult that array before writing and skip
//! any matching field.

use super::model::{CatalogueEntry, CatalogueSource, EnrichmentStatus, Field};
use crate::enrichment::MergedFields;
use chrono::{DateTime, Utc};
use sqlx::{types::ipnetwork::IpNetwork, PgPool};
use std::net::IpAddr;
use uuid::Uuid;

/// Split of newly-inserted vs already-existing rows from a bulk paste.
#[derive(Debug, Default)]
pub struct InsertOutcome {
    /// Rows that did not exist before this call.
    pub created: Vec<CatalogueEntry>,
    /// Rows that were already present; returned so callers can show the
    /// existing enrichment state without a second round-trip.
    pub existing: Vec<CatalogueEntry>,
}

/// Three-valued sentinel encoding the wire semantics of a PATCH field:
/// - outer `None`          — leave column untouched
/// - outer `Some(None)`    — set column to NULL
/// - outer `Some(Some(v))` — set column to `v`
pub type PatchValue<T> = Option<Option<T>>;

/// Operator-supplied patch to a single catalogue row.
///
/// The outer `Option` encodes "did the caller touch this field?". The
/// inner `Option` encodes "does the caller want the value set to NULL?".
/// See [`PatchValue`] for the wire semantics.
#[derive(Debug, Default)]
pub struct PatchSet {
    /// New display_name (see [`PatchValue`]).
    pub display_name: PatchValue<String>,
    /// New city.
    pub city: PatchValue<String>,
    /// New ISO 3166-1 alpha-2 country code (must be length 2 when set).
    pub country_code: PatchValue<String>,
    /// New country human-readable name.
    pub country_name: PatchValue<String>,
    /// New latitude (-90..=90).
    pub latitude: PatchValue<f64>,
    /// New longitude (-180..=180).
    pub longitude: PatchValue<f64>,
    /// New ASN.
    pub asn: PatchValue<i32>,
    /// New network operator / ISP name.
    pub network_operator: PatchValue<String>,
    /// New website URL.
    pub website: PatchValue<String>,
    /// New operator notes.
    pub notes: PatchValue<String>,
    /// Fields the operator wants re-opened for enrichment. The listed
    /// columns are set NULL and removed from `operator_edited_fields` so
    /// the next enrichment run can populate them.
    pub revert_to_auto: Vec<Field>,
}

/// Bulk idempotent insert.
///
/// Uses `ON CONFLICT (ip) DO NOTHING` to tolerate concurrent paste
/// requests without turning a unique-violation into a 500. Rows that
/// already existed are re-fetched and returned under `existing`.
pub async fn insert_many(
    pool: &PgPool,
    ips: &[IpAddr],
    source: CatalogueSource,
    created_by: Option<&str>,
) -> Result<InsertOutcome, sqlx::Error> {
    if ips.is_empty() {
        return Ok(InsertOutcome::default());
    }

    let ipnets: Vec<IpNetwork> = ips.iter().copied().map(IpNetwork::from).collect();

    let created_rows = sqlx::query_as!(
        CatalogueEntryRow,
        r#"
        INSERT INTO ip_catalogue (ip, source, created_by)
        SELECT ip, $2::catalogue_source, $3
        FROM UNNEST($1::inet[]) AS ip
        ON CONFLICT (ip) DO NOTHING
        RETURNING
            id,
            ip AS "ip: IpNetwork",
            display_name, city, country_code, country_name,
            latitude, longitude, asn, network_operator, website, notes,
            enrichment_status AS "enrichment_status: EnrichmentStatus",
            enriched_at,
            operator_edited_fields,
            source AS "source: CatalogueSource",
            created_at, created_by
        "#,
        &ipnets as &[IpNetwork],
        source as CatalogueSource,
        created_by,
    )
    .fetch_all(pool)
    .await?;

    let created: Vec<CatalogueEntry> = created_rows.into_iter().map(Into::into).collect();
    let created_ips: std::collections::HashSet<IpAddr> = created.iter().map(|e| e.ip).collect();

    let missing_ips: Vec<IpNetwork> = ips
        .iter()
        .filter(|ip| !created_ips.contains(ip))
        .copied()
        .map(IpNetwork::from)
        .collect();

    let existing = if missing_ips.is_empty() {
        Vec::new()
    } else {
        let existing_rows = sqlx::query_as!(
            CatalogueEntryRow,
            r#"
            SELECT
                id,
                ip AS "ip: IpNetwork",
                display_name, city, country_code, country_name,
                latitude, longitude, asn, network_operator, website, notes,
                enrichment_status AS "enrichment_status: EnrichmentStatus",
                enriched_at,
                operator_edited_fields,
                source AS "source: CatalogueSource",
                created_at, created_by
            FROM ip_catalogue
            WHERE ip = ANY($1::inet[])
            "#,
            &missing_ips as &[IpNetwork],
        )
        .fetch_all(pool)
        .await?;
        existing_rows.into_iter().map(Into::into).collect()
    };

    Ok(InsertOutcome { created, existing })
}

/// Look up a row by primary key.
pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<CatalogueEntry>, sqlx::Error> {
    let row = sqlx::query_as!(
        CatalogueEntryRow,
        r#"
        SELECT
            id,
            ip AS "ip: IpNetwork",
            display_name, city, country_code, country_name,
            latitude, longitude, asn, network_operator, website, notes,
            enrichment_status AS "enrichment_status: EnrichmentStatus",
            enriched_at,
            operator_edited_fields,
            source AS "source: CatalogueSource",
            created_at, created_by
        FROM ip_catalogue
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Look up a row by IP.
pub async fn find_by_ip(pool: &PgPool, ip: IpAddr) -> Result<Option<CatalogueEntry>, sqlx::Error> {
    let ipnet = IpNetwork::from(ip);
    let row = sqlx::query_as!(
        CatalogueEntryRow,
        r#"
        SELECT
            id,
            ip AS "ip: IpNetwork",
            display_name, city, country_code, country_name,
            latitude, longitude, asn, network_operator, website, notes,
            enrichment_status AS "enrichment_status: EnrichmentStatus",
            enriched_at,
            operator_edited_fields,
            source AS "source: CatalogueSource",
            created_at, created_by
        FROM ip_catalogue
        WHERE ip = $1
        "#,
        ipnet,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Remove a row. Idempotent: missing rows are not an error.
///
/// Returns the number of rows affected by the delete so callers can
/// distinguish "removed a real row" from "no-op because the id was
/// already absent" — used by the HTTP layer to suppress the SSE
/// `Deleted` event on idempotent repeats.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM ip_catalogue WHERE id = $1", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Apply an operator patch.
///
/// - Fields supplied in `patch` (outer `Some`) are written and appended to
///   `operator_edited_fields` — that is the only override-gate mechanism.
/// - Fields in `revert_to_auto` are set NULL and removed from
///   `operator_edited_fields` so the next enrichment run re-populates them.
pub async fn patch(
    pool: &PgPool,
    id: Uuid,
    patch: PatchSet,
) -> Result<CatalogueEntry, sqlx::Error> {
    let mut added_fields: Vec<String> = Vec::new();
    if patch.display_name.is_some() {
        added_fields.push(Field::DisplayName.as_str().to_string());
    }
    if patch.city.is_some() {
        added_fields.push(Field::City.as_str().to_string());
    }
    if patch.country_code.is_some() {
        added_fields.push(Field::CountryCode.as_str().to_string());
    }
    if patch.country_name.is_some() {
        added_fields.push(Field::CountryName.as_str().to_string());
    }
    if patch.latitude.is_some() {
        added_fields.push(Field::Latitude.as_str().to_string());
    }
    if patch.longitude.is_some() {
        added_fields.push(Field::Longitude.as_str().to_string());
    }
    if patch.asn.is_some() {
        added_fields.push(Field::Asn.as_str().to_string());
    }
    if patch.network_operator.is_some() {
        added_fields.push(Field::NetworkOperator.as_str().to_string());
    }
    if patch.website.is_some() {
        added_fields.push(Field::Website.as_str().to_string());
    }
    if patch.notes.is_some() {
        added_fields.push(Field::Notes.as_str().to_string());
    }
    let removed_fields: Vec<String> = patch
        .revert_to_auto
        .iter()
        .map(|f| f.as_str().to_string())
        .collect();

    // For `revert_to_auto`, null the column too.
    let clear_display_name = patch.revert_to_auto.contains(&Field::DisplayName);
    let clear_city = patch.revert_to_auto.contains(&Field::City);
    let clear_country_code = patch.revert_to_auto.contains(&Field::CountryCode);
    let clear_country_name = patch.revert_to_auto.contains(&Field::CountryName);
    let clear_latitude = patch.revert_to_auto.contains(&Field::Latitude);
    let clear_longitude = patch.revert_to_auto.contains(&Field::Longitude);
    let clear_asn = patch.revert_to_auto.contains(&Field::Asn);
    let clear_network_operator = patch.revert_to_auto.contains(&Field::NetworkOperator);
    let clear_website = patch.revert_to_auto.contains(&Field::Website);
    let clear_notes = patch.revert_to_auto.contains(&Field::Notes);

    let country_code_value = patch.country_code.clone().flatten();

    // Triple-state CASE:
    //   $Nset = "caller wrote this field"
    //   $Nclr = "caller asked to clear this field (revert_to_auto)"
    //   $Nval = new value (may be NULL to intentionally clear)
    //
    // revert_to_auto wins over set: if the operator simultaneously clears
    // and writes, we treat the clear as authoritative (a consistency the
    // API layer should not expose anyway).
    let row = sqlx::query_as!(
        CatalogueEntryRow,
        r#"
        UPDATE ip_catalogue SET
            display_name = CASE
                WHEN $2::bool THEN NULL
                WHEN $3::bool THEN $4::text
                ELSE display_name END,
            city = CASE
                WHEN $5::bool THEN NULL
                WHEN $6::bool THEN $7::text
                ELSE city END,
            country_code = CASE
                WHEN $8::bool THEN NULL
                WHEN $9::bool THEN $10::char(2)
                ELSE country_code END,
            country_name = CASE
                WHEN $11::bool THEN NULL
                WHEN $12::bool THEN $13::text
                ELSE country_name END,
            latitude = CASE
                WHEN $14::bool THEN NULL
                WHEN $15::bool THEN $16::double precision
                ELSE latitude END,
            longitude = CASE
                WHEN $17::bool THEN NULL
                WHEN $18::bool THEN $19::double precision
                ELSE longitude END,
            asn = CASE
                WHEN $20::bool THEN NULL
                WHEN $21::bool THEN $22::integer
                ELSE asn END,
            network_operator = CASE
                WHEN $23::bool THEN NULL
                WHEN $24::bool THEN $25::text
                ELSE network_operator END,
            website = CASE
                WHEN $26::bool THEN NULL
                WHEN $27::bool THEN $28::text
                ELSE website END,
            notes = CASE
                WHEN $29::bool THEN NULL
                WHEN $30::bool THEN $31::text
                ELSE notes END,
            operator_edited_fields = ARRAY(
                SELECT DISTINCT f
                FROM UNNEST(
                    ARRAY(SELECT UNNEST(operator_edited_fields) EXCEPT SELECT UNNEST($32::text[]))
                    || $33::text[]
                ) AS f
            )
        WHERE id = $1
        RETURNING
            id,
            ip AS "ip: IpNetwork",
            display_name, city, country_code, country_name,
            latitude, longitude, asn, network_operator, website, notes,
            enrichment_status AS "enrichment_status: EnrichmentStatus",
            enriched_at,
            operator_edited_fields,
            source AS "source: CatalogueSource",
            created_at, created_by
        "#,
        id,
        clear_display_name,
        patch.display_name.is_some(),
        patch.display_name.flatten(),
        clear_city,
        patch.city.is_some(),
        patch.city.flatten(),
        clear_country_code,
        patch.country_code.is_some(),
        country_code_value,
        clear_country_name,
        patch.country_name.is_some(),
        patch.country_name.flatten(),
        clear_latitude,
        patch.latitude.is_some(),
        patch.latitude.flatten(),
        clear_longitude,
        patch.longitude.is_some(),
        patch.longitude.flatten(),
        clear_asn,
        patch.asn.is_some(),
        patch.asn.flatten(),
        clear_network_operator,
        patch.network_operator.is_some(),
        patch.network_operator.flatten(),
        clear_website,
        patch.website.is_some(),
        patch.website.flatten(),
        clear_notes,
        patch.notes.is_some(),
        patch.notes.flatten(),
        &removed_fields as &[String],
        &added_fields as &[String],
    )
    .fetch_one(pool)
    .await?;

    Ok(row.into())
}

/// Reset a row to `pending` before the runner walks the provider chain.
///
/// Clears `enriched_at` so the UI can distinguish "currently being
/// enriched" from "previously enriched but now being re-run". Idempotent:
/// calling on a row already in `pending` with no prior enrichment is a
/// no-op at the value level.
pub async fn mark_enrichment_start(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE ip_catalogue SET
            enrichment_status = 'pending'::enrichment_status,
            enriched_at       = NULL
        WHERE id = $1
        "#,
        id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Bulk variant of [`mark_enrichment_start`] for bulk re-enrichment.
///
/// Unknown ids in `ids` silently no-op (the `WHERE id = ANY(...)` clause
/// simply matches no row). Returns the number of rows actually flipped
/// so the caller can distinguish "all ids resolved" from "some were
/// speculative" if that ever matters — current callers ignore the value.
pub async fn mark_enrichment_start_bulk(pool: &PgPool, ids: &[Uuid]) -> Result<u64, sqlx::Error> {
    if ids.is_empty() {
        return Ok(0);
    }
    let res = sqlx::query!(
        r#"
        UPDATE ip_catalogue SET
            enrichment_status = 'pending'::enrichment_status,
            enriched_at       = NULL
        WHERE id = ANY($1::uuid[])
        "#,
        ids,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Persist a [`MergedFields`] result onto the catalogue row.
///
/// Only populated (`Some(_)`) columns on `merged` are written; absent
/// entries are preserved via `COALESCE($N, <column>)`. `enrichment_status`
/// flips to `enriched` when at least one field was populated (per
/// [`MergedFields::any_populated`]) and `failed` otherwise. `enriched_at`
/// is stamped to `NOW()` unconditionally — the timestamp marks when the
/// pipeline last finished, regardless of outcome.
///
/// Returns `Some(terminal_status)` when the UPDATE affected a row, so
/// the caller can publish a progress event without a second round-trip.
/// Returns `None` when no row was updated — the id was concurrently
/// deleted between the runner's `find_by_id` / `mark_enrichment_start`
/// and this UPDATE. The runner uses the `None` case to suppress the
/// `EnrichmentProgress` broadcast so SSE subscribers do not see ghost
/// status updates for rows that no longer exist.
///
/// Lock enforcement happens at two points: [`MergedFields::apply`] skips
/// provider writes against the locked set the *runner* snapshotted before
/// lookup, and the `CASE … WHEN 'Foo' = ANY(operator_edited_fields)` guards
/// below *re-check at write time* against the row's current lock set.
/// The second check closes the race where an operator PATCH adds a lock
/// mid-lookup: even if the runner's snapshot was pre-lock, the UPDATE
/// observes the freshly-committed `operator_edited_fields` and preserves
/// the operator's value.
///
/// The operator-only columns `display_name`, `website`, and `notes` are
/// intentionally not touched by this function — they are never populated
/// by enrichment providers. Code adding a new enrichable field to
/// [`MergedFields`] must also add the corresponding `CASE`-guarded `SET`
/// column here (and mirror the lock name with [`Field::as_str`]) or the
/// new field will silently fail to persist.
pub async fn apply_enrichment_result(
    pool: &PgPool,
    id: Uuid,
    merged: MergedFields,
    empty_status: EnrichmentStatus,
) -> Result<Option<EnrichmentStatus>, sqlx::Error> {
    // Populated rows are always `Enriched`. Empty rows fall back to the
    // caller's choice: the runner picks `Pending` when every provider
    // error was retryable (rate limited / transient) so the sweep gets
    // another go, and `Failed` when the chain produced only terminal
    // errors or no providers are configured.
    let status = if merged.any_populated() {
        EnrichmentStatus::Enriched
    } else {
        empty_status
    };
    let result = sqlx::query!(
        r#"
        UPDATE ip_catalogue SET
            city = CASE
                WHEN 'City' = ANY(operator_edited_fields) THEN city
                ELSE COALESCE($2, city)
            END,
            country_code = CASE
                WHEN 'CountryCode' = ANY(operator_edited_fields) THEN country_code
                ELSE COALESCE($3, country_code)
            END,
            country_name = CASE
                WHEN 'CountryName' = ANY(operator_edited_fields) THEN country_name
                ELSE COALESCE($4, country_name)
            END,
            latitude = CASE
                WHEN 'Latitude' = ANY(operator_edited_fields) THEN latitude
                ELSE COALESCE($5, latitude)
            END,
            longitude = CASE
                WHEN 'Longitude' = ANY(operator_edited_fields) THEN longitude
                ELSE COALESCE($6, longitude)
            END,
            asn = CASE
                WHEN 'Asn' = ANY(operator_edited_fields) THEN asn
                ELSE COALESCE($7, asn)
            END,
            network_operator = CASE
                WHEN 'NetworkOperator' = ANY(operator_edited_fields) THEN network_operator
                ELSE COALESCE($8, network_operator)
            END,
            enrichment_status = $9::enrichment_status,
            enriched_at       = NOW()
        WHERE id = $1
        "#,
        id,
        merged.city,
        merged.country_code,
        merged.country_name,
        merged.latitude,
        merged.longitude,
        merged.asn,
        merged.network_operator,
        status as EnrichmentStatus,
    )
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        // The row was deleted between the runner's lookup and this
        // UPDATE. Surface `None` so the runner can suppress the
        // `EnrichmentProgress` broadcast — a `Deleted` SSE frame is
        // already in flight and emitting a second-hand progress event
        // for a gone row would mislead clients.
        return Ok(None);
    }
    Ok(Some(status))
}

/// Upsert a row driven by an agent's self-report on register.
///
/// Creates the row if missing (source `agent_registration`); otherwise
/// overwrites latitude/longitude. In both cases, `Latitude` / `Longitude`
/// are appended to `operator_edited_fields` so enrichment providers never
/// replace the agent's self-report.
///
/// Accepts any sqlx executor so the caller can drive this from a pool or
/// from inside an open transaction — the agent register path bundles it
/// with the `agents` upsert in a single transaction to rule out a
/// partial-commit where the agent row persists but the catalogue row
/// fails to write.
pub async fn ensure_from_agent<'e, E>(
    executor: E,
    ip: IpAddr,
    lat: f64,
    lon: f64,
) -> Result<CatalogueEntry, sqlx::Error>
where
    E: sqlx::PgExecutor<'e>,
{
    let ipnet = IpNetwork::from(ip);
    let row = sqlx::query_as!(
        CatalogueEntryRow,
        r#"
        INSERT INTO ip_catalogue (ip, source, latitude, longitude, operator_edited_fields)
        VALUES ($1::inet, 'agent_registration'::catalogue_source, $2, $3,
                ARRAY['Latitude', 'Longitude']::text[])
        ON CONFLICT (ip) DO UPDATE SET
            latitude  = EXCLUDED.latitude,
            longitude = EXCLUDED.longitude,
            operator_edited_fields = ARRAY(
                SELECT DISTINCT f
                FROM UNNEST(
                    ip_catalogue.operator_edited_fields
                    || ARRAY['Latitude', 'Longitude']::text[]
                ) AS f
            )
        RETURNING
            id,
            ip AS "ip: IpNetwork",
            display_name, city, country_code, country_name,
            latitude, longitude, asn, network_operator, website, notes,
            enrichment_status AS "enrichment_status: EnrichmentStatus",
            enriched_at,
            operator_edited_fields,
            source AS "source: CatalogueSource",
            created_at, created_by
        "#,
        ipnet,
        lat,
        lon,
    )
    .fetch_one(executor)
    .await?;
    Ok(row.into())
}

// --- List ------------------------------------------------------------------

/// Filter set accepted by [`list`].
///
/// Empty `Vec` filters mean "no restriction" — the query compiles this
/// as `$N::<elem>[] = '{}' OR column = ANY($N::<elem>[])`. `ip_prefix`
/// accepts any Postgres-parseable CIDR or bare IP; an unparseable value
/// is treated as "no filter" (falling through to `$N IS NULL`). The
/// bounding box is `[minLat, minLon, maxLat, maxLon]`.
///
/// ### Shapes
///
/// When [`ListFilter::shapes`] is non-empty the repo layer uses the
/// polygon-union bbox as a cheap SQL pre-filter (intersected with
/// [`ListFilter::bounding_box`] when both are set) and runs exact
/// point-in-polygon over the returned page via
/// [`super::shapes::point_in_any`]. SQL can only reason about the bbox,
/// so `total` is an **upper-bound approximation** whenever `shapes` is
/// non-empty: rows inside the bbox but outside every polygon are counted
/// in `total` while the PIP pass drops them from the returned page.
/// Clients that need the exact post-shape count must sum page sizes
/// across `next_cursor` walks.
///
/// ### Cursor
///
/// `after` carries the keyset cursor from a prior page. The repo layer
/// silently discards a cursor whose `(sort, dir)` does not match the
/// request's `(sort, dir)` — i.e. a sort-change invalidates in-flight
/// cursors and the request is served as a fresh page. The caller
/// (handler) is responsible for decoding the wire form via
/// [`super::sort::Cursor::decode`] and for converting decode errors
/// into "ignore and serve first page".
#[derive(Debug, Default)]
pub struct ListFilter {
    /// Exact country-code match, ANY semantics across the vector.
    pub country_code: Vec<String>,
    /// Exact ASN match, ANY semantics across the vector.
    pub asn: Vec<i32>,
    /// Case-insensitive ILIKE match on `network_operator`, ANY across
    /// the vector. Substrings must be wrapped in `%…%` by the caller
    /// when substring matching is intended — this function does not add
    /// wildcards for you.
    pub network: Vec<String>,
    /// CIDR / single-IP containment (`ip <<= $prefix`). The contained-or-equal
    /// operator is deliberate: a bare host like `1.2.3.4` parses to
    /// `1.2.3.4/32`, which `<<` (strict containment) would fail to match
    /// against the stored `/32` row. Unparseable values are ignored (no
    /// filter applied).
    pub ip_prefix: Option<String>,
    /// Case-insensitive ILIKE match on `display_name`. Wildcards are
    /// the caller's responsibility, as above.
    pub name: Option<String>,
    /// `[minLat, minLon, maxLat, maxLon]` geographic bounding box.
    pub bounding_box: Option<[f64; 4]>,
    /// Exact city match, ANY semantics across the vector.
    pub city: Vec<String>,
    /// Polygon shapes (point-in-any OR semantics). The union bbox feeds
    /// the SQL pre-filter; exact PIP runs in Rust after the page returns.
    /// Malformed polygons (fewer than 3 distinct vertices) are silently
    /// dropped by the wire layer before reaching this struct.
    pub shapes: Vec<super::dto::Polygon>,
    /// Sort column.
    pub sort: super::dto::SortBy,
    /// Sort direction.
    pub sort_dir: super::dto::SortDir,
    /// Keyset cursor from a prior page. The repo discards a cursor whose
    /// `(sort, dir)` disagrees with the request's `(sort, dir)` — see the
    /// struct doc for the rationale.
    pub after: Option<super::sort::Cursor>,
    /// Max rows to return. Clamped to [`LIST_MAX_LIMIT`] internally.
    pub limit: i64,
}

/// Maximum page size for [`list`] — larger inputs are silently clamped.
pub const LIST_MAX_LIMIT: i64 = 500;

/// List catalogue rows matching `filter`.
///
/// Returns `(rows, total, next_cursor)`.
///
/// - `rows` — up to `filter.limit.clamp(1, LIST_MAX_LIMIT)` rows ordered
///   by the selected `(sort, sort_dir)` with `id DESC` as the invariant
///   tiebreaker and `NULLS LAST` for every column regardless of
///   direction. When `filter.shapes` is non-empty the page is
///   additionally filtered in Rust by [`super::shapes::point_in_any`];
///   rows with NULL lat/lng are dropped from the shapes post-filter
///   since they cannot satisfy a geographic predicate.
/// - `total` — `COUNT(*)` over the same WHERE (excluding the cursor
///   predicate and the Rust PIP). When `filter.shapes` is non-empty
///   `total` is an upper-bound approximation — see the struct doc.
/// - `next_cursor` — derived from the **SQL-returned** last row when
///   the SQL page is exactly `limit` long. The PIP post-filter can
///   shrink the returned page below `limit` without terminating the
///   walk: the client keeps following `next_cursor` until the SQL
///   layer returns fewer than `limit` rows (signalling exhaustion).
pub async fn list(
    pool: &PgPool,
    filter: ListFilter,
) -> Result<(Vec<CatalogueEntry>, i64, Option<super::sort::Cursor>), sqlx::Error> {
    use super::dto::{SortBy, SortDir};

    let limit = filter.limit.clamp(1, LIST_MAX_LIMIT);

    // `ip_prefix` may be user-supplied; tolerate unparseable values by
    // dropping the filter rather than 400ing. Downstream SQL handles
    // `NULL` as "no filter" via `$N::INET IS NULL`.
    let ip_prefix: Option<IpNetwork> = filter
        .ip_prefix
        .as_deref()
        .and_then(|s| s.parse::<IpNetwork>().ok());

    // Bounding box: intersect the request-supplied bbox (if any) with
    // the shapes' union bbox (if any). The INTERSECTION semantics are
    // load-bearing — both filters must hold for a row to match, which
    // matches the AND-of-filters posture of every other list filter.
    let shapes_bbox = super::shapes::union_bbox(&filter.shapes);
    let bbox = match (filter.bounding_box, shapes_bbox) {
        (Some(a), Some(b)) => Some([
            a[0].max(b[0]),
            a[1].max(b[1]),
            a[2].min(b[2]),
            a[3].min(b[3]),
        ]),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    let (bbox_set, min_lat, min_lon, max_lat, max_lon) = match bbox {
        Some([a, b, c, d]) => (true, Some(a), Some(b), Some(c), Some(d)),
        None => (false, None, None, None, None),
    };

    let name_like = filter.name.as_deref();
    let country_code: &[String] = &filter.country_code;
    let asn: &[i32] = &filter.asn;
    let network: &[String] = &filter.network;
    let city: &[String] = &filter.city;

    // Cursor is silently discarded when the `(sort, dir)` doesn't agree
    // with the request — a sort change invalidates in-flight cursors and
    // the request is served as a fresh page. This keeps the client-side
    // state machine simple: flip the sort, forget the cursor.
    //
    // A second gate rejects cursors whose `value` JSON type doesn't match
    // what the sort column expects (e.g. `value: "Berlin"` arrives with
    // `sort = Asn`). Without this, the typed decode below collapses to
    // `Option::None`, which Postgres reduces to `col IS NULL` — silently
    // jumping the client to the NULLS LAST tail instead of serving a
    // fresh page. Same posture as a sort-mismatched cursor.
    let effective_after: Option<&super::sort::Cursor> = filter
        .after
        .as_ref()
        .filter(|c| c.sort == filter.sort && c.dir == filter.sort_dir)
        .filter(|c| cursor_value_matches_sort(&c.value, filter.sort));

    let after_set = effective_after.is_some();
    let after_null = effective_after.map(|c| c.value.is_null()).unwrap_or(false);
    let after_id: Option<Uuid> = effective_after.map(|c| c.id);

    // Expand one (sort, dir) pair into a fully-parameterised
    // `sqlx::query_as!`. The macro composes the SQL via sqlx's built-in
    // `LitStr + LitStr + ...` concatenation syntax — concat!()-based
    // composition does NOT work because sqlx's proc-macro parses its
    // `source` with `Punctuated::<LitStr, Token![+]>::parse_separated_nonempty`,
    // which rejects any token that isn't a string literal.
    //
    // The keyset WHERE handles three cases in one SQL template:
    //   1. No cursor present               → $12::BOOL = FALSE short-circuits.
    //   2. Cursor with NULL-tail value     → $13::BOOL enables the
    //      "col IS NULL AND c.id < $15" arm (continues the NULLS LAST tail).
    //   3. Cursor with concrete value      → normal `col <cmp_op> $14`
    //      keyset plus the same-value tiebreaker and the "col IS NULL"
    //      spill for rows in the NULLS LAST tail.
    // The `id DESC` tiebreaker is invariant across both sort directions;
    // we always page "downward" through a stable id ordering regardless
    // of whether the sort column is ascending or descending.
    //
    // `$sort_col_sql` is substituted as a literal SQL fragment (e.g.
    // `"c.city"` or `"(c.latitude IS NOT NULL AND c.longitude IS NOT NULL)"`
    // for the derived `Location` sort). NOT NULL columns (ip,
    // enrichment_status, created_at, location) render the `col IS NULL`
    // branches dead; they stay in the template for uniformity — the
    // planner removes them.
    macro_rules! list_variant {
        (
            sort_col   = $sort_col_sql:literal,
            dir        = $dir_sql:literal,
            cmp_op     = $cmp_op:literal,
            value_cast = $value_cast:literal,
            value_ty   = $value_ty:ty,
            after_val  = $after_val:expr $(,)?
        ) => {{
            let after_value_typed: Option<$value_ty> = $after_val;
            sqlx::query_as!(
                CatalogueEntryRow,
                "SELECT c.id, c.ip AS \"ip: IpNetwork\", c.display_name, c.city, c.country_code, c.country_name, c.latitude, c.longitude, c.asn, c.network_operator, c.website, c.notes, c.enrichment_status AS \"enrichment_status: EnrichmentStatus\", c.enriched_at, c.operator_edited_fields, c.source AS \"source: CatalogueSource\", c.created_at, c.created_by FROM ip_catalogue c WHERE "
                + "($1::TEXT[] = '{}' OR c.country_code = ANY($1::TEXT[])) AND "
                + "($2::INT[]  = '{}' OR c.asn          = ANY($2::INT[])) AND "
                + "($3::TEXT[] = '{}' OR c.network_operator ILIKE ANY($3::TEXT[])) AND "
                + "($4::INET IS NULL OR c.ip <<= $4::INET) AND "
                + "($5::TEXT IS NULL OR c.display_name ILIKE $5::TEXT) AND "
                + "(NOT $6::BOOL OR ("
                + "  c.latitude  BETWEEN $7::DOUBLE PRECISION AND $9::DOUBLE PRECISION AND "
                + "  c.longitude BETWEEN $8::DOUBLE PRECISION AND $10::DOUBLE PRECISION "
                + ")) AND "
                + "($11::TEXT[] = '{}' OR c.city = ANY($11::TEXT[])) AND "
                + "($12::BOOL = FALSE OR ("
                + "  ($13::BOOL AND " + $sort_col_sql + " IS NULL AND c.id < $15::UUID) OR "
                + "  (NOT $13::BOOL AND ("
                + "    " + $sort_col_sql + " " + $cmp_op + " $14::" + $value_cast + " OR "
                + "    (" + $sort_col_sql + " = $14::" + $value_cast + " AND c.id < $15::UUID) OR "
                + "    " + $sort_col_sql + " IS NULL"
                + "  ))"
                + ")) "
                + "ORDER BY " + $sort_col_sql + " " + $dir_sql + " NULLS LAST, c.id DESC "
                + "LIMIT $16",
                country_code as &[String],
                asn as &[i32],
                network as &[String],
                ip_prefix as Option<IpNetwork>,
                name_like,
                bbox_set,
                min_lat,
                min_lon,
                max_lat,
                max_lon,
                city as &[String],
                after_set,
                after_null,
                after_value_typed as Option<$value_ty>,
                after_id as Option<Uuid>,
                limit,
            )
            .fetch_all(pool)
            .await
        }};
    }

    // Decode the cursor's `value` into the type the current sort column
    // expects. Shape-incoherent cursors (wrong JSON type for the column)
    // are treated as "no cursor" — same posture as a sort-mismatched
    // cursor. Keeps the client-side state machine simple and keeps
    // malformed inputs from reaching the query layer.
    let val_i32: Option<i32> = effective_after
        .and_then(|c| c.value.as_i64())
        .and_then(|n| i32::try_from(n).ok());
    let val_str: Option<String> =
        effective_after.and_then(|c| c.value.as_str().map(str::to_string));
    let val_bool: Option<bool> = effective_after.and_then(|c| c.value.as_bool());
    let val_ts: Option<DateTime<Utc>> = effective_after
        .and_then(|c| c.value.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let rows_sql = match (filter.sort, filter.sort_dir) {
        (SortBy::CreatedAt, SortDir::Desc) => list_variant!(
            sort_col   = "c.created_at",
            dir        = "DESC",
            cmp_op     = "<",
            value_cast = "TIMESTAMPTZ",
            value_ty   = DateTime<Utc>,
            after_val  = val_ts,
        )?,
        (SortBy::CreatedAt, SortDir::Asc) => list_variant!(
            sort_col   = "c.created_at",
            dir        = "ASC",
            cmp_op     = ">",
            value_cast = "TIMESTAMPTZ",
            value_ty   = DateTime<Utc>,
            after_val  = val_ts,
        )?,
        (SortBy::Ip, SortDir::Desc) => list_variant!(
            sort_col = "host(c.ip)",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::Ip, SortDir::Asc) => list_variant!(
            sort_col = "host(c.ip)",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::DisplayName, SortDir::Desc) => list_variant!(
            sort_col = "c.display_name",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::DisplayName, SortDir::Asc) => list_variant!(
            sort_col = "c.display_name",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::City, SortDir::Desc) => list_variant!(
            sort_col = "c.city",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::City, SortDir::Asc) => list_variant!(
            sort_col = "c.city",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::CountryCode, SortDir::Desc) => list_variant!(
            sort_col = "c.country_code",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::CountryCode, SortDir::Asc) => list_variant!(
            sort_col = "c.country_code",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::Asn, SortDir::Desc) => list_variant!(
            sort_col = "c.asn",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "INTEGER",
            value_ty = i32,
            after_val = val_i32,
        )?,
        (SortBy::Asn, SortDir::Asc) => list_variant!(
            sort_col = "c.asn",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "INTEGER",
            value_ty = i32,
            after_val = val_i32,
        )?,
        (SortBy::NetworkOperator, SortDir::Desc) => list_variant!(
            sort_col = "c.network_operator",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::NetworkOperator, SortDir::Asc) => list_variant!(
            sort_col = "c.network_operator",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        // `enrichment_status` is cast to `text` so the order is
        // alphabetical on the rendered value (`enriched` < `failed` <
        // `pending`), not Postgres enum declaration order. This gives
        // operators a stable, obvious sort when staring at the column
        // header and keeps the cursor value a plain JSON string.
        (SortBy::EnrichmentStatus, SortDir::Desc) => list_variant!(
            sort_col = "c.enrichment_status::text",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::EnrichmentStatus, SortDir::Asc) => list_variant!(
            sort_col = "c.enrichment_status::text",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::Website, SortDir::Desc) => list_variant!(
            sort_col = "c.website",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::Website, SortDir::Asc) => list_variant!(
            sort_col = "c.website",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "TEXT",
            value_ty = String,
            after_val = val_str,
        )?,
        (SortBy::Location, SortDir::Desc) => list_variant!(
            sort_col = "(c.latitude IS NOT NULL AND c.longitude IS NOT NULL)",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "BOOLEAN",
            value_ty = bool,
            after_val = val_bool,
        )?,
        (SortBy::Location, SortDir::Asc) => list_variant!(
            sort_col = "(c.latitude IS NOT NULL AND c.longitude IS NOT NULL)",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "BOOLEAN",
            value_ty = bool,
            after_val = val_bool,
        )?,
    };

    // Derive the next cursor from the SQL-layer page (pre-PIP). When
    // the SQL page is shorter than `limit` the result set is exhausted
    // and there's no further cursor; otherwise encode the last row's
    // sort-column value plus its id. See the function doc for why we
    // use the SQL-layer row here, not the post-PIP row.
    let next_cursor = if rows_sql.len() as i64 == limit {
        rows_sql.last().map(|last| super::sort::Cursor {
            sort: filter.sort,
            dir: filter.sort_dir,
            value: cursor_value_for_sort(last, filter.sort),
            id: last.id,
        })
    } else {
        None
    };

    // Point-in-polygon post-filter. Pre-convert each wire polygon once
    // so the row loop doesn't re-pay the conversion cost. Polygons that
    // fail `TryFrom` (fewer than 3 distinct vertices) are dropped —
    // the wire layer already rejects those, so any failure here is a
    // defensive safeguard rather than a normal path.
    let rows: Vec<CatalogueEntry> = if filter.shapes.is_empty() {
        rows_sql.into_iter().map(Into::into).collect()
    } else {
        let polys: Vec<geo::Polygon<f64>> = filter
            .shapes
            .iter()
            .filter_map(|p| geo::Polygon::<f64>::try_from(p).ok())
            .collect();
        rows_sql
            .into_iter()
            .filter(|r| match (r.latitude, r.longitude) {
                (Some(lat), Some(lng)) => super::shapes::point_in_any(&polys, lat, lng),
                _ => false,
            })
            .map(Into::into)
            .collect()
    };

    // Total count — shared WHERE with the keyset stripped. Uses the same
    // filter surface (including the shapes bbox pre-filter) so it lines
    // up with the entries returned, modulo the PIP post-filter (see the
    // struct doc for the "approximate when shapes is non-empty" caveat).
    let total = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "total!"
        FROM ip_catalogue c
        WHERE ($1::TEXT[] = '{}' OR c.country_code = ANY($1::TEXT[]))
          AND ($2::INT[]  = '{}' OR c.asn          = ANY($2::INT[]))
          AND ($3::TEXT[] = '{}' OR c.network_operator ILIKE ANY($3::TEXT[]))
          AND ($4::INET IS NULL OR c.ip <<= $4::INET)
          AND ($5::TEXT IS NULL OR c.display_name ILIKE $5::TEXT)
          AND (NOT $6::BOOL OR (
                c.latitude  BETWEEN $7::DOUBLE PRECISION AND $9::DOUBLE PRECISION
            AND c.longitude BETWEEN $8::DOUBLE PRECISION AND $10::DOUBLE PRECISION
          ))
          AND ($11::TEXT[] = '{}' OR c.city = ANY($11::TEXT[]))
        "#,
        country_code as &[String],
        asn as &[i32],
        network as &[String],
        ip_prefix as Option<IpNetwork>,
        name_like,
        bbox_set,
        min_lat,
        min_lon,
        max_lat,
        max_lon,
        city as &[String],
    )
    .fetch_one(pool)
    .await?;

    Ok((rows, total, next_cursor))
}

/// Extract the sort-column value from a row as a `serde_json::Value` for
/// embedding in the next cursor. Nullable columns return `Value::Null`;
/// the derived `Location` sort returns a `Value::Bool`.
fn cursor_value_for_sort(row: &CatalogueEntryRow, sort: super::dto::SortBy) -> serde_json::Value {
    use super::dto::SortBy;
    use serde_json::Value;
    match sort {
        SortBy::CreatedAt => Value::String(row.created_at.to_rfc3339()),
        SortBy::Ip => Value::String(row.ip.ip().to_string()),
        SortBy::DisplayName => row
            .display_name
            .as_ref()
            .map(|s| Value::String(s.clone()))
            .unwrap_or(Value::Null),
        SortBy::City => row
            .city
            .as_ref()
            .map(|s| Value::String(s.clone()))
            .unwrap_or(Value::Null),
        SortBy::CountryCode => row
            .country_code
            .as_ref()
            .map(|s| Value::String(s.clone()))
            .unwrap_or(Value::Null),
        SortBy::Asn => row
            .asn
            .map(|a| Value::Number(a.into()))
            .unwrap_or(Value::Null),
        SortBy::NetworkOperator => row
            .network_operator
            .as_ref()
            .map(|s| Value::String(s.clone()))
            .unwrap_or(Value::Null),
        SortBy::EnrichmentStatus => Value::String(match row.enrichment_status {
            EnrichmentStatus::Pending => "pending".to_string(),
            EnrichmentStatus::Enriched => "enriched".to_string(),
            EnrichmentStatus::Failed => "failed".to_string(),
        }),
        SortBy::Website => row
            .website
            .as_ref()
            .map(|s| Value::String(s.clone()))
            .unwrap_or(Value::Null),
        SortBy::Location => Value::Bool(row.latitude.is_some() && row.longitude.is_some()),
    }
}

/// True when a cursor's `value` JSON type is coherent with the sort
/// column. `Value::Null` is always accepted — it signals the NULLS LAST
/// tail and is valid for every column. The accepted non-null types
/// mirror the output of [`cursor_value_for_sort`]:
///
/// | Sort column                                                   | Expected `value` type |
/// |---------------------------------------------------------------|-----------------------|
/// | `CreatedAt`, `Ip`, `DisplayName`, `City`, `CountryCode`,      | `String`              |
/// | `NetworkOperator`, `EnrichmentStatus`, `Website`              |                       |
/// | `Asn`                                                         | `Number`              |
/// | `Location`                                                    | `Bool`                |
///
/// A mismatch (e.g. `value: "Berlin"` sent against `sort = Asn`)
/// returns `false` so the repo layer can discard the cursor and serve
/// a fresh page — same posture as a `(sort, dir)` mismatch. Without
/// this gate the typed decode collapses to `None`, which Postgres
/// reduces to `col IS NULL`, silently jumping the client to the NULLS
/// LAST tail instead of resetting to the first page.
fn cursor_value_matches_sort(value: &serde_json::Value, sort: super::dto::SortBy) -> bool {
    use super::dto::SortBy;
    if value.is_null() {
        return true;
    }
    match sort {
        SortBy::CreatedAt
        | SortBy::Ip
        | SortBy::DisplayName
        | SortBy::City
        | SortBy::CountryCode
        | SortBy::NetworkOperator
        | SortBy::EnrichmentStatus
        | SortBy::Website => value.is_string(),
        SortBy::Asn => value.is_number(),
        SortBy::Location => value.is_boolean(),
    }
}

// --- Facets ----------------------------------------------------------------

/// Per-country occurrence count.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct CountryFacet {
    /// ISO 3166-1 alpha-2 country code.
    pub code: String,
    /// Human-readable country name when available.
    pub name: Option<String>,
    /// Number of rows with this country_code.
    pub count: i64,
}

/// Per-ASN occurrence count.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AsnFacet {
    /// Autonomous system number.
    pub asn: i32,
    /// Number of rows with this ASN.
    pub count: i64,
}

/// Per-network-operator occurrence count.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct NetworkFacet {
    /// Network operator / ISP name.
    pub name: String,
    /// Number of rows with this operator.
    pub count: i64,
}

/// Per-city occurrence count.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct CityFacet {
    /// City name.
    pub name: String,
    /// Number of rows with this city.
    pub count: i64,
}

/// Aggregate facets used by the catalogue's filter UI.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct FacetsResponse {
    /// Top 250 country buckets, descending by count.
    pub countries: Vec<CountryFacet>,
    /// Top 250 ASN buckets, descending by count.
    pub asns: Vec<AsnFacet>,
    /// Top 250 operator buckets, descending by count.
    pub networks: Vec<NetworkFacet>,
    /// Top 250 city buckets, descending by count.
    pub cities: Vec<CityFacet>,
}

/// Compute catalogue facets in a single round-trip.
pub async fn facets(pool: &PgPool) -> Result<FacetsResponse, sqlx::Error> {
    let countries = sqlx::query_as!(
        CountryFacet,
        r#"
        SELECT country_code AS "code!",
               MAX(country_name) AS name,
               COUNT(*) AS "count!"
        FROM ip_catalogue
        WHERE country_code IS NOT NULL
        GROUP BY country_code
        ORDER BY COUNT(*) DESC
        LIMIT 250
        "#,
    )
    .fetch_all(pool)
    .await?;

    let asns = sqlx::query_as!(
        AsnFacet,
        r#"
        SELECT asn AS "asn!", COUNT(*) AS "count!"
        FROM ip_catalogue
        WHERE asn IS NOT NULL
        GROUP BY asn
        ORDER BY COUNT(*) DESC
        LIMIT 250
        "#,
    )
    .fetch_all(pool)
    .await?;

    let networks = sqlx::query_as!(
        NetworkFacet,
        r#"
        SELECT network_operator AS "name!", COUNT(*) AS "count!"
        FROM ip_catalogue
        WHERE network_operator IS NOT NULL
        GROUP BY network_operator
        ORDER BY COUNT(*) DESC
        LIMIT 250
        "#,
    )
    .fetch_all(pool)
    .await?;

    let cities = sqlx::query_as!(
        CityFacet,
        r#"
        SELECT city AS "name!", COUNT(*) AS "count!"
        FROM ip_catalogue
        WHERE city IS NOT NULL
        GROUP BY city
        ORDER BY COUNT(*) DESC
        LIMIT 250
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(FacetsResponse {
        countries,
        asns,
        networks,
        cities,
    })
}

// --- Row mirror ------------------------------------------------------------

/// Flat sqlx row mirror of [`CatalogueEntry`]. Exists because sqlx's macro
/// cannot populate nested structs directly.
struct CatalogueEntryRow {
    id: Uuid,
    ip: IpNetwork,
    display_name: Option<String>,
    city: Option<String>,
    country_code: Option<String>,
    country_name: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    asn: Option<i32>,
    network_operator: Option<String>,
    website: Option<String>,
    notes: Option<String>,
    enrichment_status: EnrichmentStatus,
    enriched_at: Option<DateTime<Utc>>,
    operator_edited_fields: Vec<String>,
    source: CatalogueSource,
    created_at: DateTime<Utc>,
    created_by: Option<String>,
}

impl From<CatalogueEntryRow> for CatalogueEntry {
    fn from(r: CatalogueEntryRow) -> Self {
        Self {
            id: r.id,
            ip: r.ip.ip(),
            display_name: r.display_name,
            city: r.city,
            country_code: r.country_code,
            country_name: r.country_name,
            latitude: r.latitude,
            longitude: r.longitude,
            asn: r.asn,
            network_operator: r.network_operator,
            website: r.website,
            notes: r.notes,
            enrichment_status: r.enrichment_status,
            enriched_at: r.enriched_at,
            operator_edited_fields: r.operator_edited_fields,
            source: r.source,
            created_at: r.created_at,
            created_by: r.created_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::dto::SortBy;
    use super::*;
    use serde_json::json;

    /// Every sort column accepts a `Value::Null` cursor value — that's
    /// how a cursor in the NULLS LAST tail is represented.
    #[test]
    fn cursor_value_matches_sort_accepts_null_for_every_sort() {
        for sort in [
            SortBy::CreatedAt,
            SortBy::Ip,
            SortBy::DisplayName,
            SortBy::City,
            SortBy::CountryCode,
            SortBy::Asn,
            SortBy::NetworkOperator,
            SortBy::EnrichmentStatus,
            SortBy::Website,
            SortBy::Location,
        ] {
            assert!(
                cursor_value_matches_sort(&serde_json::Value::Null, sort),
                "Null must be accepted for {sort:?}"
            );
        }
    }

    /// String-typed columns accept a `Value::String` cursor and reject
    /// every other non-null JSON type.
    #[test]
    fn cursor_value_matches_sort_accepts_string_for_string_columns() {
        let string_sorts = [
            SortBy::CreatedAt,
            SortBy::Ip,
            SortBy::DisplayName,
            SortBy::City,
            SortBy::CountryCode,
            SortBy::NetworkOperator,
            SortBy::EnrichmentStatus,
            SortBy::Website,
        ];
        for sort in string_sorts {
            assert!(cursor_value_matches_sort(&json!("Berlin"), sort));
            assert!(!cursor_value_matches_sort(&json!(42), sort));
            assert!(!cursor_value_matches_sort(&json!(true), sort));
        }
    }

    /// `Asn` accepts numbers and rejects strings — a cursor minted
    /// against `City` whose value is `"Berlin"` must not survive when
    /// the request's sort flips to `Asn`.
    #[test]
    fn cursor_value_matches_sort_asn_rejects_string_value() {
        assert!(cursor_value_matches_sort(&json!(64500), SortBy::Asn));
        assert!(!cursor_value_matches_sort(&json!("Berlin"), SortBy::Asn));
        assert!(!cursor_value_matches_sort(&json!(true), SortBy::Asn));
    }

    /// `Location` accepts booleans and rejects everything non-null.
    #[test]
    fn cursor_value_matches_sort_location_requires_bool() {
        assert!(cursor_value_matches_sort(&json!(true), SortBy::Location));
        assert!(cursor_value_matches_sort(&json!(false), SortBy::Location));
        assert!(!cursor_value_matches_sort(
            &json!("Berlin"),
            SortBy::Location
        ));
        assert!(!cursor_value_matches_sort(&json!(1), SortBy::Location));
    }

    /// Regression pin for the gate's reason for existing: a cursor
    /// that *matches* on `(sort, dir)` but whose `value` has the wrong
    /// JSON type for the requested column must be rejected. This mirrors
    /// the `(SortBy::Asn, Value::String("Berlin"))` scenario from the
    /// T51 Task 3 review: without the gate, the typed decode collapses
    /// to `None`, Postgres reduces `col > NULL OR (col = NULL AND …)`
    /// to `col IS NULL`, and the client silently lands in the NULLS
    /// LAST tail instead of serving a fresh page.
    #[test]
    fn cursor_with_mismatched_value_type_rejected_like_no_cursor() {
        // A cursor minted against `City` with value "Berlin", replayed
        // against a request whose sort is `Asn`. `(sort, dir)` would be
        // `(Asn, Desc)` on both sides (the prior gate passes), but the
        // value-type gate must reject.
        let bad = json!("Berlin");
        assert!(!cursor_value_matches_sort(&bad, SortBy::Asn));
        // The "no cursor" posture corresponds to `Value::Null`; a Null
        // value on `Asn` is permitted because it means the NULL-tail.
        // So a bad cursor and a no-cursor request diverge on what
        // reaches the query layer, but both result in "serve a fresh
        // page" — the bad cursor is dropped before any typed decode.
        assert!(cursor_value_matches_sort(
            &serde_json::Value::Null,
            SortBy::Asn
        ));
    }
}
