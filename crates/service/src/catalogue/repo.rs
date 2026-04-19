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
) -> Result<Option<EnrichmentStatus>, sqlx::Error> {
    let status = if merged.any_populated() {
        EnrichmentStatus::Enriched
    } else {
        EnrichmentStatus::Failed
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
pub async fn ensure_from_agent(
    pool: &PgPool,
    ip: IpAddr,
    lat: f64,
    lon: f64,
) -> Result<CatalogueEntry, sqlx::Error> {
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
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

// --- List ------------------------------------------------------------------

/// Filter set accepted by [`list`].
///
/// Empty `Vec` filters mean "no restriction" — the query compiles this
/// as `$N::TEXT[] = '{}' OR column = ANY($N::TEXT[])`. `ip_prefix`
/// accepts any Postgres-parseable CIDR or bare IP; an unparseable value
/// is treated as "no filter" (falling through to `$N IS NULL`). The
/// bounding box is `[minLat, minLon, maxLat, maxLon]`.
///
/// `cursor_created_at` / `cursor_id` are accepted for forward
/// compatibility with T13 (cursor-paginated list). T11 ignores them —
/// the list returns the first `limit.min(500)` rows sorted by
/// `created_at DESC, id DESC` and a separate `COUNT(*)` over the same
/// filter set.
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
    /// Max rows to return. Clamped to `500` internally.
    pub limit: i64,
    /// TODO(T13): cursor pagination. Ignored in T11.
    pub cursor_created_at: Option<DateTime<Utc>>,
    /// TODO(T13): cursor pagination. Ignored in T11.
    pub cursor_id: Option<Uuid>,
}

/// Maximum page size for [`list`] — larger inputs are silently clamped.
pub const LIST_MAX_LIMIT: i64 = 500;

/// List catalogue rows matching `filter`, returning the rows and the
/// unpaged `COUNT(*)` over the same WHERE clauses.
///
/// T11 returns the first `limit.min(LIST_MAX_LIMIT)` matching rows in
/// `(created_at DESC, id DESC)` order. Cursor pagination is deferred to
/// T13 (see [`ListFilter::cursor_created_at`]).
pub async fn list(
    pool: &PgPool,
    filter: ListFilter,
) -> Result<(Vec<CatalogueEntry>, i64), sqlx::Error> {
    let limit = filter.limit.clamp(1, LIST_MAX_LIMIT);

    // `ip_prefix` may be user-supplied; tolerate unparseable values by
    // dropping the filter rather than 400ing. Downstream SQL handles
    // `NULL` as "no filter" via `$N::INET IS NULL`.
    let ip_prefix: Option<IpNetwork> = filter
        .ip_prefix
        .as_deref()
        .and_then(|s| s.parse::<IpNetwork>().ok());

    // Bounding box parts. When absent or all-NULL, the SQL arm collapses
    // to the always-true `$bbox_set IS NULL` branch.
    let (bbox_set, min_lat, min_lon, max_lat, max_lon) = match filter.bounding_box {
        Some([a, b, c, d]) => (true, Some(a), Some(b), Some(c), Some(d)),
        None => (false, None, None, None, None),
    };

    let name_like = filter.name.as_deref();

    let rows = sqlx::query_as!(
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
        FROM ip_catalogue c
        WHERE ($1::TEXT[] = '{}' OR country_code = ANY($1::TEXT[]))
          AND ($2::INT[]  = '{}' OR asn          = ANY($2::INT[]))
          AND ($3::TEXT[] = '{}' OR network_operator ILIKE ANY($3::TEXT[]))
          AND ($4::INET IS NULL OR c.ip <<= $4::INET)
          AND ($5::TEXT IS NULL OR display_name ILIKE $5::TEXT)
          AND (NOT $6::BOOL OR (
                latitude  BETWEEN $7::DOUBLE PRECISION AND $9::DOUBLE PRECISION
            AND longitude BETWEEN $8::DOUBLE PRECISION AND $10::DOUBLE PRECISION
          ))
        ORDER BY created_at DESC, id DESC
        LIMIT $11
        "#,
        &filter.country_code as &[String],
        &filter.asn as &[i32],
        &filter.network as &[String],
        ip_prefix as Option<IpNetwork>,
        name_like,
        bbox_set,
        min_lat,
        min_lon,
        max_lat,
        max_lon,
        limit,
    )
    .fetch_all(pool)
    .await?;

    let total = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "total!"
        FROM ip_catalogue c
        WHERE ($1::TEXT[] = '{}' OR country_code = ANY($1::TEXT[]))
          AND ($2::INT[]  = '{}' OR asn          = ANY($2::INT[]))
          AND ($3::TEXT[] = '{}' OR network_operator ILIKE ANY($3::TEXT[]))
          AND ($4::INET IS NULL OR c.ip <<= $4::INET)
          AND ($5::TEXT IS NULL OR display_name ILIKE $5::TEXT)
          AND (NOT $6::BOOL OR (
                latitude  BETWEEN $7::DOUBLE PRECISION AND $9::DOUBLE PRECISION
            AND longitude BETWEEN $8::DOUBLE PRECISION AND $10::DOUBLE PRECISION
          ))
        "#,
        &filter.country_code as &[String],
        &filter.asn as &[i32],
        &filter.network as &[String],
        ip_prefix as Option<IpNetwork>,
        name_like,
        bbox_set,
        min_lat,
        min_lon,
        max_lat,
        max_lon,
    )
    .fetch_one(pool)
    .await?;

    Ok((rows.into_iter().map(Into::into).collect(), total))
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
