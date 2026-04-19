//! Integration tests for the `catalogue::repo` CRUD layer.
//!
//! Uses `common::acquire(false)` so each test gets a throwaway database —
//! catalogue rows are globally-unique on `ip`, so sharing a DB between
//! parallel tests would risk conflicts on `10.0.0.*` fixtures.

mod common;

use meshmon_service::catalogue::{
    model::{CatalogueSource, EnrichmentStatus, Field},
    repo,
};
use meshmon_service::enrichment::MergedFields;
use std::net::IpAddr;

#[tokio::test]
async fn insert_many_is_idempotent_on_ip() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["1.1.1.1".parse().unwrap(), "2.2.2.2".parse().unwrap()];
    let first = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    assert_eq!(first.created.len(), 2);
    assert!(first.existing.is_empty());
    for row in &first.created {
        assert_eq!(row.source, CatalogueSource::Operator);
        assert_eq!(row.enrichment_status, EnrichmentStatus::Pending);
    }

    let second = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    assert!(second.created.is_empty());
    assert_eq!(second.existing.len(), 2);

    db.close().await;
}

#[tokio::test]
async fn insert_many_returns_empty_outcome_for_empty_input() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let out = repo::insert_many(&db.pool, &[], CatalogueSource::Operator, None)
        .await
        .unwrap();
    assert!(out.created.is_empty());
    assert!(out.existing.is_empty());

    db.close().await;
}

#[tokio::test]
async fn patch_appends_fields_to_operator_edited() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["3.3.3.3".parse().unwrap()];
    let ins = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    let id = ins.created[0].id;

    let patched = repo::patch(
        &db.pool,
        id,
        repo::PatchSet {
            display_name: Some(Some("fastly-sfo".into())),
            city: Some(Some("San Francisco".into())),
            country_code: Some(Some("US".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(patched.display_name.as_deref(), Some("fastly-sfo"));
    assert_eq!(patched.city.as_deref(), Some("San Francisco"));
    assert_eq!(patched.country_code.as_deref(), Some("US"));

    for expected in [Field::DisplayName, Field::City, Field::CountryCode] {
        assert!(
            patched
                .operator_edited_fields
                .iter()
                .any(|f| f == expected.as_str()),
            "expected {} in operator_edited_fields but got {:?}",
            expected.as_str(),
            patched.operator_edited_fields
        );
    }

    db.close().await;
}

#[tokio::test]
async fn patch_revert_to_auto_clears_value_and_field() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["3.3.3.4".parse().unwrap()];
    let ins = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    let id = ins.created[0].id;

    // Operator writes city
    let patched = repo::patch(
        &db.pool,
        id,
        repo::PatchSet {
            city: Some(Some("Paris".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(patched.is_locked(Field::City));

    // Operator reverts city
    let reverted = repo::patch(
        &db.pool,
        id,
        repo::PatchSet {
            revert_to_auto: vec![Field::City],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(reverted.city, None);
    assert!(!reverted.is_locked(Field::City));

    db.close().await;
}

#[tokio::test]
async fn patch_does_not_duplicate_operator_edited_entries() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["3.3.3.5".parse().unwrap()];
    let ins = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    let id = ins.created[0].id;

    for _ in 0..3 {
        repo::patch(
            &db.pool,
            id,
            repo::PatchSet {
                display_name: Some(Some("fastly-sfo".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    let row = repo::find_by_id(&db.pool, id).await.unwrap().unwrap();
    let hits = row
        .operator_edited_fields
        .iter()
        .filter(|f| f == &Field::DisplayName.as_str())
        .count();
    assert_eq!(hits, 1);

    db.close().await;
}

#[tokio::test]
async fn ensure_from_agent_marks_lat_lon_as_edited() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ip: IpAddr = "9.9.9.9".parse().unwrap();
    let entry = repo::ensure_from_agent(&db.pool, ip, 37.7749, -122.4194)
        .await
        .unwrap();
    assert_eq!(entry.latitude, Some(37.7749));
    assert_eq!(entry.longitude, Some(-122.4194));
    assert_eq!(entry.source, CatalogueSource::AgentRegistration);
    assert!(entry.is_locked(Field::Latitude));
    assert!(entry.is_locked(Field::Longitude));

    // Second call updates the coords but still holds the lock — no duplicates.
    let updated = repo::ensure_from_agent(&db.pool, ip, 40.0, -73.0)
        .await
        .unwrap();
    assert_eq!(updated.latitude, Some(40.0));
    assert_eq!(updated.longitude, Some(-73.0));
    let lat_hits = updated
        .operator_edited_fields
        .iter()
        .filter(|f| f == &Field::Latitude.as_str())
        .count();
    assert_eq!(lat_hits, 1);

    db.close().await;
}

#[tokio::test]
async fn delete_removes_row() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["4.4.4.4".parse().unwrap()];
    let ins = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    let id = ins.created[0].id;
    repo::delete(&db.pool, id).await.unwrap();
    assert!(repo::find_by_id(&db.pool, id).await.unwrap().is_none());
    // Deleting a missing row is a no-op, not an error.
    repo::delete(&db.pool, id).await.unwrap();

    db.close().await;
}

#[tokio::test]
async fn find_by_ip_round_trips_row() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ip: IpAddr = "5.6.7.8".parse().unwrap();
    let ins = repo::insert_many(&db.pool, &[ip], CatalogueSource::Operator, Some("alice"))
        .await
        .unwrap();
    assert_eq!(ins.created[0].created_by.as_deref(), Some("alice"));

    let hit = repo::find_by_ip(&db.pool, ip).await.unwrap().unwrap();
    assert_eq!(hit.id, ins.created[0].id);
    assert_eq!(hit.created_by.as_deref(), Some("alice"));

    let miss = repo::find_by_ip(&db.pool, "5.6.7.9".parse().unwrap())
        .await
        .unwrap();
    assert!(miss.is_none());

    db.close().await;
}

#[tokio::test]
async fn facets_aggregates_counts_per_column() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    for (ip, cc, name) in [
        ("10.0.0.1", "DE", "Germany"),
        ("10.0.0.2", "DE", "Germany"),
        ("10.0.0.3", "US", "United States"),
    ] {
        sqlx::query(
            "INSERT INTO ip_catalogue (ip, source, enrichment_status, country_code, country_name)
             VALUES ($1::inet, 'operator', 'enriched', $2, $3)",
        )
        .bind(ip)
        .bind(cc)
        .bind(name)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    let facets = repo::facets(&db.pool).await.unwrap();
    assert!(facets
        .countries
        .iter()
        .any(|c| c.code == "DE" && c.count == 2 && c.name.as_deref() == Some("Germany")));
    assert!(facets
        .countries
        .iter()
        .any(|c| c.code == "US" && c.count == 1));
    // Empty buckets are skipped entirely (WHERE .. IS NOT NULL).
    assert!(facets.asns.is_empty());
    assert!(facets.networks.is_empty());
    assert!(facets.cities.is_empty());

    db.close().await;
}

/// Regression guard for the operator-lock race closed by the `CASE …
/// ANY(operator_edited_fields)` guards in `apply_enrichment_result`.
///
/// Scenario: an operator PATCH adds `City` to the lock set *after* the
/// runner snapshotted `operator_edited_fields` but *before* the runner
/// writes its merged result. With only the runner-side snapshot check
/// (the pre-fix code) the write would still overwrite the operator's
/// City. With the DB-side write-time re-check, the UPDATE observes the
/// freshly-committed lock and preserves the operator's value.
///
/// We simulate the race by calling `apply_enrichment_result` directly
/// with a populated `city` AFTER seeding the row with
/// `operator_edited_fields = ['City']` and a specific operator value.
#[tokio::test]
async fn apply_enrichment_respects_locks_committed_after_snapshot() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["10.9.9.9".parse().unwrap()];
    let out = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    let id = out.created[0].id;

    // Seed the row to the state an operator PATCH would produce mid-
    // lookup: `city` set + `City` lock present. No other fields locked.
    sqlx::query(
        "UPDATE ip_catalogue
         SET city = 'OperatorCity',
             operator_edited_fields = ARRAY['City']::text[]
         WHERE id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();

    // Merged fields a provider might want to write — `city = FromProvider`
    // must be IGNORED because `City` is now locked at DB level, even
    // though the runner's pre-lookup snapshot was empty.
    let mut merged = MergedFields::default();
    merged.apply(
        "test-provider",
        meshmon_service::enrichment::EnrichmentResult {
            fields: [
                (
                    Field::City,
                    meshmon_service::enrichment::FieldValue::Text("FromProvider".into()),
                ),
                (
                    Field::CountryCode,
                    meshmon_service::enrichment::FieldValue::Text("US".into()),
                ),
            ]
            .into_iter()
            .collect(),
        },
        // empty locked set — the runner's pre-lookup snapshot.
        &[],
    );

    let terminal = repo::apply_enrichment_result(&db.pool, id, merged, EnrichmentStatus::Failed)
        .await
        .unwrap()
        .expect("row exists so UPDATE must touch it");
    assert_eq!(terminal, EnrichmentStatus::Enriched);

    // Assert: city survived (write-time CASE saw the lock); country_code
    // landed (not locked).
    let row: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT city, country_code FROM ip_catalogue WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        row.0.as_deref(),
        Some("OperatorCity"),
        "operator lock added after snapshot must survive the write",
    );
    assert_eq!(
        row.1.as_deref(),
        Some("US"),
        "unlocked field must still be written",
    );

    db.close().await;
}

/// Regression guard for the concurrent-delete race in
/// `apply_enrichment_result`: if the row is deleted between the runner's
/// lookup and this UPDATE, the function must return `Ok(None)` instead
/// of `Ok(Some(status))` so the caller skips the progress broadcast and
/// doesn't emit ghost `EnrichmentProgress` events for gone rows.
#[tokio::test]
async fn apply_enrichment_returns_none_when_row_deleted_concurrently() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["10.99.99.99".parse().unwrap()];
    let out = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    let id = out.created[0].id;

    // Delete the row before the "runner" calls apply_enrichment_result.
    let deleted = repo::delete(&db.pool, id).await.unwrap();
    assert_eq!(deleted, 1);

    // Now simulate the runner racing: call apply_enrichment_result on
    // the now-missing id. The UPDATE touches zero rows and the function
    // must return `Ok(None)` so the caller suppresses the SSE broadcast.
    let mut merged = MergedFields::default();
    merged.apply(
        "test-provider",
        meshmon_service::enrichment::EnrichmentResult {
            fields: [(
                Field::City,
                meshmon_service::enrichment::FieldValue::Text("GhostCity".into()),
            )]
            .into_iter()
            .collect(),
        },
        &[],
    );
    let result = repo::apply_enrichment_result(&db.pool, id, merged, EnrichmentStatus::Failed)
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "apply_enrichment_result on a deleted row must return Ok(None), got {result:?}",
    );

    db.close().await;
}
