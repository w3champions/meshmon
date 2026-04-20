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

/// Operator-set default metadata applied during a bulk paste.
///
/// Paired fields (`country_code`+`country_name`, `latitude`+
/// `longitude`) are expected to be supplied together — the paste
/// handler rejects half-supplied pairs before reaching the repo, so
/// the repo's paired-atomicity logic can assume either both halves are
/// `Some` or both are `None`. Each optional field is independent of
/// the others.
#[derive(Debug, Clone, Default)]
pub struct BulkMetadata {
    /// Operator-facing display label.
    pub display_name: Option<String>,
    /// City name.
    pub city: Option<String>,
    /// ISO 3166-1 alpha-2 country code. Paired with `country_name`.
    pub country_code: Option<String>,
    /// Country human-readable name. Paired with `country_code`.
    pub country_name: Option<String>,
    /// Decimal latitude. Paired with `longitude`.
    pub latitude: Option<f64>,
    /// Decimal longitude. Paired with `latitude`.
    pub longitude: Option<f64>,
    /// Operator-supplied external link.
    pub website: Option<String>,
    /// Free-form operator notes.
    pub notes: Option<String>,
}

impl BulkMetadata {
    /// True when no field is set. Caller can skip the merge pass
    /// entirely in this case.
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.city.is_none()
            && self.country_code.is_none()
            && self.latitude.is_none()
            && self.website.is_none()
            && self.notes.is_none()
    }
}

/// Outcome of [`insert_many_with_metadata`].
///
/// `existing` entries reflect the **post-merge** row state so callers
/// do not need a follow-up fetch. `skips` carries a per-row log of
/// fields (or paired-field labels `"Location"` / `"Country"`) that
/// were refused because they were already in
/// `operator_edited_fields`. Rows whose metadata was applied cleanly
/// do not appear in `skips`; rows where every supplied field was
/// skipped appear with their full skip list.
#[derive(Debug, Default)]
pub struct BulkInsertOutcome {
    /// Newly-inserted rows, with any supplied metadata already applied
    /// and the corresponding field names appended to
    /// `operator_edited_fields`.
    pub created: Vec<CatalogueEntry>,
    /// Rows that were already present, each carrying its post-merge
    /// state (values on unlocked fields, prior values on locked ones).
    pub existing: Vec<CatalogueEntry>,
    /// Per-row skip log — only present for rows that refused at least
    /// one metadata write. `"Location"` / `"Country"` appear as
    /// composite keys for paired-field skips.
    pub skips: Vec<(Uuid, Vec<String>)>,
    /// Ids of existing rows whose merge actually wrote at least one
    /// column. Lets the handler fan out `Updated` SSE events without
    /// double-counting rows whose supplied fields were all locked.
    pub updated_existing: std::collections::HashSet<Uuid>,
}

/// Per-row write plan distilled from a [`BulkMetadata`] and the
/// target row's current `operator_edited_fields`.
#[derive(Debug, Default)]
struct RowWritePlan {
    display_name: Option<String>,
    city: Option<String>,
    country_code: Option<String>,
    country_name: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    website: Option<String>,
    notes: Option<String>,
    /// Canonical field names to append to `operator_edited_fields`.
    added_locks: Vec<String>,
    /// Field / pair labels this plan refused to write because the
    /// target field was already locked.
    skipped: Vec<String>,
}

impl RowWritePlan {
    /// Compute the per-row plan for a given metadata + existing
    /// lock set. Paired fields apply atomically — when either half
    /// is already locked, both halves are dropped from the write and
    /// a composite `"Location"` / `"Country"` entry is appended to
    /// `skipped`.
    fn compute(md: &BulkMetadata, locks: &[String]) -> Self {
        let locked: std::collections::HashSet<&str> = locks.iter().map(String::as_str).collect();
        let mut plan = RowWritePlan::default();

        fn apply_single(
            plan_field: &mut Option<String>,
            plan_added: &mut Vec<String>,
            plan_skipped: &mut Vec<String>,
            locked: &std::collections::HashSet<&str>,
            field: Field,
            value: &Option<String>,
        ) {
            if let Some(v) = value.as_ref() {
                if locked.contains(field.as_str()) {
                    plan_skipped.push(field.as_str().to_string());
                } else {
                    *plan_field = Some(v.clone());
                    plan_added.push(field.as_str().to_string());
                }
            }
        }

        apply_single(
            &mut plan.display_name,
            &mut plan.added_locks,
            &mut plan.skipped,
            &locked,
            Field::DisplayName,
            &md.display_name,
        );
        apply_single(
            &mut plan.city,
            &mut plan.added_locks,
            &mut plan.skipped,
            &locked,
            Field::City,
            &md.city,
        );
        apply_single(
            &mut plan.website,
            &mut plan.added_locks,
            &mut plan.skipped,
            &locked,
            Field::Website,
            &md.website,
        );
        apply_single(
            &mut plan.notes,
            &mut plan.added_locks,
            &mut plan.skipped,
            &locked,
            Field::Notes,
            &md.notes,
        );

        // Paired: Country — atomic on CountryCode + CountryName.
        if md.country_code.is_some() {
            let locked_either = locked.contains(Field::CountryCode.as_str())
                || locked.contains(Field::CountryName.as_str());
            if locked_either {
                plan.skipped.push("Country".to_string());
            } else {
                plan.country_code = md.country_code.clone();
                plan.country_name = md.country_name.clone();
                plan.added_locks
                    .push(Field::CountryCode.as_str().to_string());
                plan.added_locks
                    .push(Field::CountryName.as_str().to_string());
            }
        }

        // Paired: Location — atomic on Latitude + Longitude.
        if md.latitude.is_some() {
            let locked_either = locked.contains(Field::Latitude.as_str())
                || locked.contains(Field::Longitude.as_str());
            if locked_either {
                plan.skipped.push("Location".to_string());
            } else {
                plan.latitude = md.latitude;
                plan.longitude = md.longitude;
                plan.added_locks.push(Field::Latitude.as_str().to_string());
                plan.added_locks.push(Field::Longitude.as_str().to_string());
            }
        }

        plan
    }

    /// True when the plan would actually write any column. Rows whose
    /// every proposed field was skipped can skip the UPDATE entirely.
    fn has_writes(&self) -> bool {
        !self.added_locks.is_empty()
    }
}

/// Apply a [`RowWritePlan`] to a single row.
///
/// Single-field SQL guards mirror [`apply_enrichment_result`] — they
/// re-check locks at write time so a concurrent operator PATCH cannot
/// silently escape the plan. Paired fields (`CountryCode`+`CountryName`,
/// `Latitude`+`Longitude`) are guarded by a **composite** predicate:
/// each half's CASE arm references both halves' lock state, so if
/// either half becomes locked between plan computation and UPDATE the
/// whole pair is skipped atomically. Without this, the narrow window
/// where a concurrent PATCH locks only one half could leave a row
/// with (say) the new `country_name` but the old `country_code` — a
/// violation of the T52 paired-atomicity contract.
///
/// The `operator_edited_fields` array is similarly filtered: the
/// supplied `added_locks` are trimmed of `CountryCode` / `CountryName`
/// when the country pair's composite guard fires, and likewise for
/// the latitude / longitude pair. This keeps the lock bookkeeping in
/// lockstep with the values it protects.
async fn apply_metadata_update(
    pool: &PgPool,
    id: Uuid,
    plan: &RowWritePlan,
) -> Result<CatalogueEntry, sqlx::Error> {
    let country_code = plan.country_code.clone();
    let row = sqlx::query_as!(
        CatalogueEntryRow,
        r#"
        UPDATE ip_catalogue SET
            display_name = CASE
                WHEN 'DisplayName' = ANY(operator_edited_fields) THEN display_name
                ELSE COALESCE($2, display_name)
            END,
            city = CASE
                WHEN 'City' = ANY(operator_edited_fields) THEN city
                ELSE COALESCE($3, city)
            END,
            country_code = CASE
                WHEN 'CountryCode' = ANY(operator_edited_fields)
                  OR 'CountryName' = ANY(operator_edited_fields)
                    THEN country_code
                ELSE COALESCE($4::char(2), country_code)
            END,
            country_name = CASE
                WHEN 'CountryCode' = ANY(operator_edited_fields)
                  OR 'CountryName' = ANY(operator_edited_fields)
                    THEN country_name
                ELSE COALESCE($5, country_name)
            END,
            latitude = CASE
                WHEN 'Latitude' = ANY(operator_edited_fields)
                  OR 'Longitude' = ANY(operator_edited_fields)
                    THEN latitude
                ELSE COALESCE($6, latitude)
            END,
            longitude = CASE
                WHEN 'Latitude' = ANY(operator_edited_fields)
                  OR 'Longitude' = ANY(operator_edited_fields)
                    THEN longitude
                ELSE COALESCE($7, longitude)
            END,
            website = CASE
                WHEN 'Website' = ANY(operator_edited_fields) THEN website
                ELSE COALESCE($8, website)
            END,
            notes = CASE
                WHEN 'Notes' = ANY(operator_edited_fields) THEN notes
                ELSE COALESCE($9, notes)
            END,
            operator_edited_fields = ARRAY(
                SELECT DISTINCT f
                FROM UNNEST(operator_edited_fields || (
                    SELECT COALESCE(array_agg(g), ARRAY[]::text[])
                    FROM UNNEST($10::text[]) AS g
                    WHERE
                        -- Drop paired-field locks when the composite
                        -- guard above already skipped the pair.
                        NOT (
                            g IN ('CountryCode', 'CountryName')
                            AND (
                                'CountryCode' = ANY(operator_edited_fields)
                                OR 'CountryName' = ANY(operator_edited_fields)
                            )
                        )
                        AND NOT (
                            g IN ('Latitude', 'Longitude')
                            AND (
                                'Latitude' = ANY(operator_edited_fields)
                                OR 'Longitude' = ANY(operator_edited_fields)
                            )
                        )
                )) AS f
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
        plan.display_name,
        plan.city,
        country_code,
        plan.country_name,
        plan.latitude,
        plan.longitude,
        plan.website,
        plan.notes,
        &plan.added_locks as &[String],
    )
    .fetch_one(pool)
    .await?;
    Ok(row.into())
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

/// Bulk idempotent insert — legacy wrapper around
/// [`insert_many_with_metadata`].
///
/// Uses `ON CONFLICT (ip) DO NOTHING` to tolerate concurrent paste
/// requests without turning a unique-violation into a 500. Rows that
/// already existed are re-fetched and returned under `existing`.
/// Retained as the default entry-point for call sites that do not
/// carry operator metadata (agent registration, the enrichment runner).
pub async fn insert_many(
    pool: &PgPool,
    ips: &[IpAddr],
    source: CatalogueSource,
    created_by: Option<&str>,
) -> Result<InsertOutcome, sqlx::Error> {
    let out =
        insert_many_with_metadata(pool, ips, source, created_by, None, &Default::default()).await?;
    Ok(InsertOutcome {
        created: out.created,
        existing: out.existing,
    })
}

/// Bulk idempotent insert with optional operator-supplied metadata.
///
/// When `md` is `None` the behaviour matches the legacy
/// [`insert_many`]: rows are inserted with `ON CONFLICT (ip) DO
/// NOTHING`, and existing rows come back untouched in the `existing`
/// bucket.
///
/// When `md` is `Some`, each supplied field applies to every accepted
/// IP:
/// - **Newly-created rows** always receive the values and have the
///   corresponding field names appended to `operator_edited_fields`.
/// - **Existing rows** receive a field only if it is not already in
///   `operator_edited_fields`. Paired fields
///   (`CountryCode`+`CountryName`, `Latitude`+`Longitude`) apply
///   atomically — if either half of a pair is locked, neither half
///   is written and the skip log records the composite label
///   (`"Country"` / `"Location"`).
///
/// The returned `existing` entries reflect the post-merge row state so
/// callers do not need a follow-up fetch.
pub async fn insert_many_with_metadata(
    pool: &PgPool,
    ips: &[IpAddr],
    source: CatalogueSource,
    created_by: Option<&str>,
    md: Option<&BulkMetadata>,
    per_ip_display_names: &std::collections::HashMap<IpAddr, String>,
) -> Result<BulkInsertOutcome, sqlx::Error> {
    if ips.is_empty() {
        return Ok(BulkInsertOutcome::default());
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

    let mut created: Vec<CatalogueEntry> = created_rows.into_iter().map(Into::into).collect();
    let created_ips: std::collections::HashSet<IpAddr> = created.iter().map(|e| e.ip).collect();

    let missing_ips: Vec<IpNetwork> = ips
        .iter()
        .filter(|ip| !created_ips.contains(ip))
        .copied()
        .map(IpNetwork::from)
        .collect();

    let mut existing: Vec<CatalogueEntry> = if missing_ips.is_empty() {
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

    let mut skips: Vec<(Uuid, Vec<String>)> = Vec::new();
    let mut updated_existing: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    // Short-circuit only when neither a global `md` nor any per-IP
    // override carries a write. `per_ip_display_names` lets the caller
    // supply per-row display names without setting the panel-wide
    // default, so an empty `md` does not imply "no work to do".
    let has_any_per_ip = !per_ip_display_names.is_empty();
    let has_any_md = md.map(|m| !m.is_empty()).unwrap_or(false);
    if has_any_md || has_any_per_ip {
        // For a given row, compute the effective [`BulkMetadata`]:
        // starts from the global default (or all-None when absent),
        // then overlays the per-IP display-name override if set for
        // this row's IP. The override wins over `md.display_name`.
        let effective_for = |row_ip: IpAddr| -> BulkMetadata {
            let mut eff = md.cloned().unwrap_or_default();
            if let Some(name) = per_ip_display_names.get(&row_ip) {
                eff.display_name = Some(name.clone());
            }
            eff
        };

        // New rows start with an empty lock set, so the plan's
        // `skipped` is always empty for this bucket — record the
        // `added_locks` and overwrite the row with the post-merge
        // state so callers see the metadata they supplied.
        for row in created.iter_mut() {
            let eff = effective_for(row.ip);
            if eff.is_empty() {
                continue;
            }
            let plan = RowWritePlan::compute(&eff, &row.operator_edited_fields);
            if plan.has_writes() {
                let updated = apply_metadata_update(pool, row.id, &plan).await?;
                *row = updated;
            }
        }

        // Existing rows may have locks; the plan encodes the per-row
        // skip decision. Issue the UPDATE only when at least one field
        // would actually be written, otherwise record the skip and
        // leave the row untouched.
        for row in existing.iter_mut() {
            let eff = effective_for(row.ip);
            if eff.is_empty() {
                continue;
            }
            let plan = RowWritePlan::compute(&eff, &row.operator_edited_fields);
            if plan.has_writes() {
                let updated = apply_metadata_update(pool, row.id, &plan).await?;
                *row = updated;
                updated_existing.insert(row.id);
            }
            if !plan.skipped.is_empty() {
                skips.push((row.id, plan.skipped.clone()));
            }
        }
    }

    Ok(BulkInsertOutcome {
        created,
        existing,
        skips,
        updated_existing,
    })
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

    // Cursor is silently discarded through three gates in sequence:
    //
    //   1. `(sort, dir)` match       — a sort change on the client
    //      invalidates in-flight cursors.
    //   2. JSON value shape match    — rejects cross-shape payloads
    //      (e.g. `"Berlin"` on `sort = Asn`). Without this, the typed
    //      decode collapses to `None` and Postgres reduces
    //      `col > NULL OR (col = NULL AND …)` to `col IS NULL`,
    //      silently jumping the client to the NULLS LAST tail.
    //   3. Typed decode success      — rejects wrong-value-inside-
    //      correct-shape payloads (`1.5` on `Asn` — valid Number but
    //      doesn't fit i32; `"not a date"` on `CreatedAt` — valid
    //      String but fails RFC3339). Same NULL-tail leak posture as
    //      gate 2 if skipped.
    //
    // All three failure modes collapse to "no cursor → fresh page";
    // the handler already treats a malformed on-the-wire cursor the
    // same way, so the client-side state machine stays simple.
    let effective_after: Option<&super::sort::Cursor> = filter
        .after
        .as_ref()
        .filter(|c| c.sort == filter.sort && c.dir == filter.sort_dir)
        .filter(|c| cursor_value_matches_sort(&c.value, filter.sort))
        .filter(|c| cursor_value_decodes_for_sort(&c.value, filter.sort));

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
    //
    // Rendered example for `(SortBy::City, SortDir::Asc)`:
    //
    // ```sql
    // SELECT c.id, c.ip AS "ip: IpNetwork", c.display_name, c.city,
    //        c.country_code, c.country_name, c.latitude, c.longitude,
    //        c.asn, c.network_operator, c.website, c.notes,
    //        c.enrichment_status AS "enrichment_status: EnrichmentStatus",
    //        c.enriched_at, c.operator_edited_fields,
    //        c.source AS "source: CatalogueSource",
    //        c.created_at, c.created_by
    // FROM ip_catalogue c
    // WHERE ($1::TEXT[] = '{}' OR c.country_code = ANY($1::TEXT[]))
    //   AND ($2::INT[]  = '{}' OR c.asn          = ANY($2::INT[]))
    //   AND ($3::TEXT[] = '{}' OR c.network_operator ILIKE ANY($3::TEXT[]))
    //   AND ($4::INET IS NULL OR c.ip <<= $4::INET)
    //   AND ($5::TEXT IS NULL OR c.display_name ILIKE $5::TEXT)
    //   AND (NOT $6::BOOL OR (
    //         c.latitude  BETWEEN $7::DOUBLE PRECISION AND $9::DOUBLE PRECISION
    //     AND c.longitude BETWEEN $8::DOUBLE PRECISION AND $10::DOUBLE PRECISION
    //   ))
    //   AND ($11::TEXT[] = '{}' OR c.city = ANY($11::TEXT[]))
    //   AND ($12::BOOL = FALSE OR (
    //         ($13::BOOL AND c.city IS NULL AND c.id < $15::UUID)
    //      OR (NOT $13::BOOL AND (
    //             c.city > $14::TEXT
    //          OR (c.city = $14::TEXT AND c.id < $15::UUID)
    //          OR c.city IS NULL))
    //   ))
    // ORDER BY c.city ASC NULLS LAST, c.id DESC
    // LIMIT $16
    // ```
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
    // expects. The three-gate chain on `effective_after` above already
    // guarantees the decode succeeds for the *current* sort; the
    // `effective_after.and_then(..)` calls below are shaped defensively
    // so a cursor whose shape or typed decode would have failed still
    // collapses to `None` (same posture as the gate itself).
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
    // `SortBy::Ip` sorts on the native `inet` column so Postgres gives
    // network-aware (not lexicographic) ordering — `10.0.0.1` sorts
    // after `9.0.0.1`, not before. The keyset binds `$14::INET`, so the
    // Rust-side value must be `IpNetwork`; the cursor's wire form stays
    // a JSON string (canonical ip form) for stability across releases.
    let val_ipnet: Option<IpNetwork> = effective_after
        .and_then(|c| c.value.as_str())
        .and_then(|s| s.parse::<IpNetwork>().ok());

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
        // `SortBy::Ip` uses native `inet` ordering (network-aware), not
        // `host(c.ip)` text ordering — `10.0.0.1 > 9.0.0.1` under inet,
        // but `"10.0.0.1" < "9.0.0.1"` lexicographically. Native inet
        // also keeps the column index-usable (`idx_ip_catalogue_*`) if
        // a future migration adds a btree on `ip`.
        (SortBy::Ip, SortDir::Desc) => list_variant!(
            sort_col = "c.ip",
            dir = "DESC",
            cmp_op = "<",
            value_cast = "INET",
            value_ty = IpNetwork,
            after_val = val_ipnet,
        )?,
        (SortBy::Ip, SortDir::Asc) => list_variant!(
            sort_col = "c.ip",
            dir = "ASC",
            cmp_op = ">",
            value_cast = "INET",
            value_ty = IpNetwork,
            after_val = val_ipnet,
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
        // `SortBy::Location` is a derived `BOOL` expression that is
        // never NULL — so `NULLS LAST` in ORDER BY and the `col IS NULL`
        // branches of the keyset are no-ops here. They stay in the
        // macro template for uniformity with the nullable columns; the
        // Postgres planner eliminates them.
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
    //
    // KEEP IN SYNC with the `list_variant!` SQL template: when a new
    // non-cursor filter column lands on `ListFilter` and gets a clause
    // in the list query, the same clause must be mirrored into this
    // COUNT so `entries.len() > 0 → total > 0` stays invariant.
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

/// True when a cursor's `value` JSON type matches the shape expected
/// for the sort column. `Value::Null` is always accepted — it signals
/// the NULLS LAST tail and is valid for every column. Non-null values
/// defer to the single source of truth on
/// [`super::sort::SortBy::cursor_value_shape`], which pairs every
/// column with exactly one JSON shape.
///
/// A mismatch (e.g. `value: "Berlin"` sent against `sort = Asn`)
/// returns `false` so the repo layer can discard the cursor and serve
/// a fresh page — same posture as a `(sort, dir)` mismatch. Without
/// this gate the typed decode collapses to `None`, which Postgres
/// reduces to `col IS NULL`, silently jumping the client to the NULLS
/// LAST tail instead of resetting to the first page.
fn cursor_value_matches_sort(value: &serde_json::Value, sort: super::dto::SortBy) -> bool {
    use super::sort::CursorValueShape;
    if value.is_null() {
        return true;
    }
    match sort.cursor_value_shape() {
        CursorValueShape::String => value.is_string(),
        CursorValueShape::Number => value.is_number(),
        CursorValueShape::Bool => value.is_boolean(),
    }
}

/// True when a cursor's non-null `value` successfully decodes into the
/// typed Rust value the sort column's SQL binding expects. Catches the
/// wrong-value-inside-correct-shape case that [`cursor_value_matches_sort`]
/// doesn't: e.g. `Asn` with `Value::Number(1.5)` (valid Number shape but
/// `as_i64()` returns `None`), `CreatedAt` with `Value::String("not a date")`
/// (valid String shape but `DateTime::parse_from_rfc3339` fails), `Ip`
/// with `Value::String("not-an-ip")` (valid String but `IpNetwork::parse`
/// fails).
///
/// Returns `true` for `Value::Null` since the caller handles NULL-tail
/// cursors on a separate SQL branch that never reads the typed value.
/// The repo layer gates `effective_after` on this function's result —
/// a `false` here folds into the same silent-discard path as the shape
/// and `(sort, dir)` mismatches.
fn cursor_value_decodes_for_sort(value: &serde_json::Value, sort: super::dto::SortBy) -> bool {
    use super::dto::SortBy;
    if value.is_null() {
        return true;
    }
    match sort {
        SortBy::CreatedAt => value
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .is_some(),
        SortBy::Ip => value
            .as_str()
            .and_then(|s| s.parse::<IpNetwork>().ok())
            .is_some(),
        SortBy::Asn => value.as_i64().and_then(|n| i32::try_from(n).ok()).is_some(),
        SortBy::DisplayName
        | SortBy::City
        | SortBy::CountryCode
        | SortBy::NetworkOperator
        | SortBy::EnrichmentStatus
        | SortBy::Website => value.is_string(),
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

// --- Map ------------------------------------------------------------------

/// Threshold separating the detail vs cluster response for
/// [`map_detail_or_clusters`]: at or below this row count we return raw
/// rows; above it we aggregate into grid buckets.
pub const MAP_DETAIL_THRESHOLD: i64 = 2000;

/// Grid cell size in degrees for [`map_detail_or_clusters`] cluster
/// aggregation, keyed by zoom level.
///
/// The band layout trades map-level visual density for cell count:
/// low-zoom views aggregate aggressively (whole continents into a few
/// cells), high-zoom views use sub-degree cells so markers don't
/// over-merge. Zooms beyond 20 fall back to the finest band.
pub fn cell_size_for_zoom(zoom: u8) -> f64 {
    match zoom {
        0..=2 => 10.0,
        3..=5 => 5.0,
        6..=8 => 1.0,
        9..=11 => 0.25,
        12..=14 => 0.05,
        _ => 0.01,
    }
}

/// Filter set accepted by [`map_detail_or_clusters`].
///
/// Mirrors [`ListFilter`] minus `sort`/`sort_dir`/`after`/`shapes`/`city`
/// and with a **required** [`bbox`](MapFilter::bbox). Shapes are omitted
/// by design — operators draw shapes against the unfiltered geography
/// of the fleet — and paging is N/A on a map view. The separate type
/// encodes those semantics at the API surface.
#[derive(Debug)]
pub struct MapFilter {
    /// Exact country-code match, ANY semantics across the vector.
    pub country_code: Vec<String>,
    /// Exact ASN match, ANY semantics across the vector.
    pub asn: Vec<i32>,
    /// Case-insensitive ILIKE match on `network_operator`, ANY across
    /// the vector. Substrings must be wrapped in `%…%` by the caller.
    pub network: Vec<String>,
    /// CIDR / single-IP containment (`ip <<= $prefix`). Unparseable
    /// values are ignored (no filter applied).
    pub ip_prefix: Option<String>,
    /// Case-insensitive ILIKE match on `display_name`. Wildcards are
    /// the caller's responsibility.
    pub name: Option<String>,
    /// Viewport `[minLat, minLon, maxLat, maxLon]`. Always applied.
    pub bbox: [f64; 4],
}

/// Adaptive map view result: raw rows below the threshold, grid-
/// aggregated clusters above it. The zoom input to
/// [`map_detail_or_clusters`] maps to a fixed cell size via
/// [`cell_size_for_zoom`].
pub enum MapResult {
    /// Raw rows — the filtered viewport count is at or below
    /// [`MAP_DETAIL_THRESHOLD`].
    Detail {
        /// Rows ordered by `(created_at DESC, id DESC)`.
        rows: Vec<CatalogueEntry>,
        /// Count of rows matching the filter in the viewport.
        total: i64,
    },
    /// Grid-aggregated buckets — the filtered viewport count exceeds
    /// [`MAP_DETAIL_THRESHOLD`].
    Clusters {
        /// One bucket per occupied grid cell.
        buckets: Vec<super::dto::MapBucket>,
        /// Count of rows matching the filter in the viewport (sum of
        /// bucket counts — `total == buckets.iter().map(|b| b.count).sum()`).
        total: i64,
        /// Cell size in degrees used for aggregation.
        cell_size: f64,
    },
}

/// Adaptive-response map query: return raw rows when the filtered
/// viewport count is small enough to render directly; otherwise,
/// grid-aggregate the matches into cell-centered buckets.
///
/// The detail/cluster split is driven by [`MAP_DETAIL_THRESHOLD`]; the
/// grid cell size is picked from [`cell_size_for_zoom`]. All three
/// underlying SQL queries share the same filter WHERE so the view
/// stays internally consistent across the threshold boundary (a row
/// present in the count is present in the detail result or aggregated
/// into a cluster bucket).
pub async fn map_detail_or_clusters(
    pool: &PgPool,
    filter: MapFilter,
    zoom: u8,
) -> Result<MapResult, sqlx::Error> {
    let total = count_in_bbox(pool, &filter).await?;
    if total <= MAP_DETAIL_THRESHOLD {
        let rows = list_detail_in_bbox(pool, &filter, MAP_DETAIL_THRESHOLD).await?;
        Ok(MapResult::Detail { rows, total })
    } else {
        let cell = cell_size_for_zoom(zoom);
        let buckets = aggregate_clusters(pool, &filter, cell).await?;
        Ok(MapResult::Clusters {
            buckets,
            total,
            cell_size: cell,
        })
    }
}

/// Count of rows matching the map filter inside the viewport.
///
/// KEEP IN SYNC with [`list_detail_in_bbox`] and [`aggregate_clusters`]:
/// all three share the same filter WHERE so the adaptive map view
/// stays internally consistent — a row counted here must be present in
/// the corresponding detail result or contribute to a cluster bucket.
///
/// The explicit `latitude IS NOT NULL AND longitude IS NOT NULL` gate
/// is redundant with the bbox BETWEEN (NULL values fail any comparison)
/// but we keep it for the `idx_ip_catalogue_latlon` index hint — the
/// planner picks the partial `(latitude, longitude)` index when it sees
/// the NOT NULL predicate explicitly.
async fn count_in_bbox(pool: &PgPool, filter: &MapFilter) -> Result<i64, sqlx::Error> {
    let country_code: &[String] = &filter.country_code;
    let asn: &[i32] = &filter.asn;
    let network: &[String] = &filter.network;
    let ip_prefix: Option<IpNetwork> = filter
        .ip_prefix
        .as_deref()
        .and_then(|s| s.parse::<IpNetwork>().ok());
    let [min_lat, min_lon, max_lat, max_lon] = filter.bbox;

    let total = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "total!"
        FROM ip_catalogue c
        WHERE ($1::TEXT[] = '{}' OR c.country_code = ANY($1::TEXT[]))
          AND ($2::INT[]  = '{}' OR c.asn          = ANY($2::INT[]))
          AND ($3::TEXT[] = '{}' OR c.network_operator ILIKE ANY($3::TEXT[]))
          AND ($4::INET IS NULL OR c.ip <<= $4::INET)
          AND ($5::TEXT IS NULL OR c.display_name ILIKE $5::TEXT)
          AND c.latitude  IS NOT NULL
          AND c.longitude IS NOT NULL
          AND c.latitude  BETWEEN $6::DOUBLE PRECISION AND $8::DOUBLE PRECISION
          AND c.longitude BETWEEN $7::DOUBLE PRECISION AND $9::DOUBLE PRECISION
        "#,
        country_code as &[String],
        asn as &[i32],
        network as &[String],
        ip_prefix as Option<IpNetwork>,
        filter.name.as_deref(),
        min_lat,
        min_lon,
        max_lat,
        max_lon,
    )
    .fetch_one(pool)
    .await?;
    Ok(total)
}

/// Return up to `limit` rows matching the filter inside the viewport,
/// ordered `(created_at DESC, id DESC)` for determinism.
///
/// Called only when [`count_in_bbox`] returned at most
/// [`MAP_DETAIL_THRESHOLD`], so in practice `limit == MAP_DETAIL_THRESHOLD`
/// — the explicit `LIMIT` acts as a belt-and-braces cap should the
/// caller ever invoke this directly.
///
/// KEEP IN SYNC with [`count_in_bbox`] and [`aggregate_clusters`].
async fn list_detail_in_bbox(
    pool: &PgPool,
    filter: &MapFilter,
    limit: i64,
) -> Result<Vec<CatalogueEntry>, sqlx::Error> {
    let country_code: &[String] = &filter.country_code;
    let asn: &[i32] = &filter.asn;
    let network: &[String] = &filter.network;
    let ip_prefix: Option<IpNetwork> = filter
        .ip_prefix
        .as_deref()
        .and_then(|s| s.parse::<IpNetwork>().ok());
    let [min_lat, min_lon, max_lat, max_lon] = filter.bbox;

    let rows = sqlx::query_as!(
        CatalogueEntryRow,
        r#"
        SELECT
            c.id,
            c.ip AS "ip: IpNetwork",
            c.display_name,
            c.city,
            c.country_code,
            c.country_name,
            c.latitude,
            c.longitude,
            c.asn,
            c.network_operator,
            c.website,
            c.notes,
            c.enrichment_status AS "enrichment_status: EnrichmentStatus",
            c.enriched_at,
            c.operator_edited_fields,
            c.source AS "source: CatalogueSource",
            c.created_at,
            c.created_by
        FROM ip_catalogue c
        WHERE ($1::TEXT[] = '{}' OR c.country_code = ANY($1::TEXT[]))
          AND ($2::INT[]  = '{}' OR c.asn          = ANY($2::INT[]))
          AND ($3::TEXT[] = '{}' OR c.network_operator ILIKE ANY($3::TEXT[]))
          AND ($4::INET IS NULL OR c.ip <<= $4::INET)
          AND ($5::TEXT IS NULL OR c.display_name ILIKE $5::TEXT)
          AND c.latitude  IS NOT NULL
          AND c.longitude IS NOT NULL
          AND c.latitude  BETWEEN $6::DOUBLE PRECISION AND $8::DOUBLE PRECISION
          AND c.longitude BETWEEN $7::DOUBLE PRECISION AND $9::DOUBLE PRECISION
        ORDER BY c.created_at DESC, c.id DESC
        LIMIT $10
        "#,
        country_code as &[String],
        asn as &[i32],
        network as &[String],
        ip_prefix as Option<IpNetwork>,
        filter.name.as_deref(),
        min_lat,
        min_lon,
        max_lat,
        max_lon,
        limit,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Aggregate matching rows into `cell_size`-degree grid cells.
///
/// Each bucket carries the cell's center coordinates, the row count,
/// a deterministic sample id (`(ARRAY_AGG(id ORDER BY id))[1]`) for
/// client-side drill-down, and the cell's bounding box as
/// `[min_lat, min_lng, max_lat, max_lng]`.
///
/// KEEP IN SYNC with [`count_in_bbox`] and [`list_detail_in_bbox`].
async fn aggregate_clusters(
    pool: &PgPool,
    filter: &MapFilter,
    cell_size: f64,
) -> Result<Vec<super::dto::MapBucket>, sqlx::Error> {
    let country_code: &[String] = &filter.country_code;
    let asn: &[i32] = &filter.asn;
    let network: &[String] = &filter.network;
    let ip_prefix: Option<IpNetwork> = filter
        .ip_prefix
        .as_deref()
        .and_then(|s| s.parse::<IpNetwork>().ok());
    let [min_lat, min_lon, max_lat, max_lon] = filter.bbox;

    // Bucket center = `FLOOR(x / cell) * cell + cell/2`. Grouping by
    // the center naturally snaps rows into the same cell. `lat_min` /
    // `lng_min` are recomputed via `MIN(FLOOR(…) * cell)` over the
    // group so the bucket's bbox is exact even if Postgres rounds the
    // center expression differently per row (it shouldn't — `FLOOR`
    // over deterministic inputs is deterministic — but the extra
    // `MIN(…)` is free under the same GROUP BY).
    struct BucketRow {
        lat_center: Option<f64>,
        lng_center: Option<f64>,
        lat_min: Option<f64>,
        lng_min: Option<f64>,
        count: i64,
        sample_id: Option<Uuid>,
    }
    let rows = sqlx::query_as!(
        BucketRow,
        r#"
        SELECT
            FLOOR(c.latitude  / $10::DOUBLE PRECISION) * $10::DOUBLE PRECISION
                + $10::DOUBLE PRECISION / 2.0
                AS "lat_center: f64",
            FLOOR(c.longitude / $10::DOUBLE PRECISION) * $10::DOUBLE PRECISION
                + $10::DOUBLE PRECISION / 2.0
                AS "lng_center: f64",
            MIN(FLOOR(c.latitude  / $10::DOUBLE PRECISION) * $10::DOUBLE PRECISION)
                AS "lat_min: f64",
            MIN(FLOOR(c.longitude / $10::DOUBLE PRECISION) * $10::DOUBLE PRECISION)
                AS "lng_min: f64",
            COUNT(*)::BIGINT                         AS "count!: i64",
            (ARRAY_AGG(c.id ORDER BY c.id))[1]       AS "sample_id: Uuid"
        FROM ip_catalogue c
        WHERE ($1::TEXT[] = '{}' OR c.country_code = ANY($1::TEXT[]))
          AND ($2::INT[]  = '{}' OR c.asn          = ANY($2::INT[]))
          AND ($3::TEXT[] = '{}' OR c.network_operator ILIKE ANY($3::TEXT[]))
          AND ($4::INET IS NULL OR c.ip <<= $4::INET)
          AND ($5::TEXT IS NULL OR c.display_name ILIKE $5::TEXT)
          AND c.latitude  IS NOT NULL
          AND c.longitude IS NOT NULL
          AND c.latitude  BETWEEN $6::DOUBLE PRECISION AND $8::DOUBLE PRECISION
          AND c.longitude BETWEEN $7::DOUBLE PRECISION AND $9::DOUBLE PRECISION
        GROUP BY
            FLOOR(c.latitude  / $10::DOUBLE PRECISION) * $10::DOUBLE PRECISION
                + $10::DOUBLE PRECISION / 2.0,
            FLOOR(c.longitude / $10::DOUBLE PRECISION) * $10::DOUBLE PRECISION
                + $10::DOUBLE PRECISION / 2.0
        "#,
        country_code as &[String],
        asn as &[i32],
        network as &[String],
        ip_prefix as Option<IpNetwork>,
        filter.name.as_deref(),
        min_lat,
        min_lon,
        max_lat,
        max_lon,
        cell_size,
    )
    .fetch_all(pool)
    .await?;

    let buckets = rows
        .into_iter()
        .filter_map(|r| {
            // Every row here has non-null lat/lng — the filter ensures
            // that — so each aggregation column is `Some` too. We unwrap
            // defensively via `filter_map` so a wayward NULL doesn't
            // panic; sqlx reports the columns as nullable because they
            // sit under aggregate/expression wrappers.
            let lat = r.lat_center?;
            let lng = r.lng_center?;
            let lat_min = r.lat_min?;
            let lng_min = r.lng_min?;
            let sample_id = r.sample_id?;
            Some(super::dto::MapBucket {
                lat,
                lng,
                count: r.count,
                sample_id,
                bbox: [lat_min, lng_min, lat_min + cell_size, lng_min + cell_size],
            })
        })
        .collect();
    Ok(buckets)
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

    /// Symmetry: a JSON number against `Location` is rejected. Fills
    /// the asymmetry the prior round of tests left in — every sort
    /// column already had at least one rejection case, but `Location`
    /// versus `Number` and `CreatedAt` versus `Bool` were implicit.
    #[test]
    fn cursor_value_matches_sort_location_rejects_number() {
        assert!(!cursor_value_matches_sort(&json!(42), SortBy::Location));
        assert!(!cursor_value_matches_sort(&json!(0.5), SortBy::Location));
    }

    /// Symmetry: a JSON boolean against `CreatedAt` is rejected. See
    /// `cursor_value_matches_sort_location_rejects_number`.
    #[test]
    fn cursor_value_matches_sort_created_at_rejects_bool() {
        assert!(!cursor_value_matches_sort(&json!(true), SortBy::CreatedAt));
        assert!(!cursor_value_matches_sort(&json!(false), SortBy::CreatedAt));
    }

    /// Wrong-value-inside-correct-shape: a `Value::Number(1.5)` passes
    /// the shape gate for `Asn` (it *is* a number) but overflows /
    /// under-resolves an `i32` at `as_i64().and_then(i32::try_from)`.
    /// Without `cursor_value_decodes_for_sort`, `val_i32` would be
    /// `None`, SQL would see `$14 = NULL`, and the keyset would
    /// collapse to `col IS NULL` — silent NULL-tail leak. With the
    /// decode gate, the whole cursor is discarded and the client
    /// gets a fresh page.
    #[test]
    fn cursor_value_decodes_for_sort_asn_rejects_out_of_range_number() {
        // Fractional: passes `as_i64` as None (not an integer).
        assert!(!cursor_value_decodes_for_sort(&json!(1.5), SortBy::Asn));
        // Beyond u64::MAX can't even parse into serde_json::Number as
        // a positive integer; exercise `i64::MAX + 1` via `u64::MAX`
        // which overflows `i32::try_from` even if `as_i64` succeeds.
        assert!(!cursor_value_decodes_for_sort(
            &json!(u64::MAX),
            SortBy::Asn
        ));
        // Valid in-range i32 still decodes.
        assert!(cursor_value_decodes_for_sort(&json!(64500), SortBy::Asn));
    }

    /// Wrong-value-inside-correct-shape: `"not-a-date"` is a `String`
    /// (passes the shape gate for `CreatedAt`) but fails
    /// `DateTime::parse_from_rfc3339`. Same silent-leak hazard as the
    /// `Asn` case above; same silent-discard treatment.
    #[test]
    fn cursor_value_decodes_for_sort_created_at_rejects_bad_rfc3339() {
        assert!(!cursor_value_decodes_for_sort(
            &json!("not-a-date"),
            SortBy::CreatedAt
        ));
        // A well-formed RFC3339 timestamp still decodes.
        assert!(cursor_value_decodes_for_sort(
            &json!("2026-04-20T12:00:00Z"),
            SortBy::CreatedAt
        ));
    }

    /// `SortBy::Ip` decodes its cursor value as `IpNetwork` so Postgres
    /// sees native `inet` ordering (network-aware). A valid IP literal
    /// round-trips; a bare string that isn't an IP is rejected by the
    /// decode gate. This pins the difference between lexicographic
    /// ordering (`"10.0.0.1" < "9.0.0.1"`) and inet ordering
    /// (`10.0.0.1 > 9.0.0.1`) — a cursor pinned at `10.0.0.1` paginates
    /// to `10.0.0.2` next, not to `2.0.0.1` (which would happen under
    /// lexicographic sort).
    #[test]
    fn cursor_value_decodes_for_sort_ip_parses_inet() {
        // The cursor's value is the canonical string form; the decode
        // gate parses it as `IpNetwork`. `10.0.0.1` parses fine and
        // becomes the inet keyset's `$14` bind.
        assert!(cursor_value_decodes_for_sort(
            &json!("10.0.0.1"),
            SortBy::Ip
        ));
        // `10.0.0.2` parses and compares as inet: `10.0.0.2 > 10.0.0.1`
        // under native inet ordering. This is the ordering the repo
        // layer now relies on (SQL column `c.ip`, not `host(c.ip)`).
        let a: IpNetwork = "10.0.0.1".parse().unwrap();
        let b: IpNetwork = "10.0.0.2".parse().unwrap();
        let c: IpNetwork = "2.0.0.1".parse().unwrap();
        assert!(b > a, "inet ordering: 10.0.0.2 > 10.0.0.1");
        assert!(a > c, "inet ordering: 10.0.0.1 > 2.0.0.1");
        // Non-IP strings are rejected.
        assert!(!cursor_value_decodes_for_sort(
            &json!("not-an-ip"),
            SortBy::Ip
        ));
    }
}
