//! Integration tests for the keyset-paging + column-sort rewrite of
//! [`meshmon_service::catalogue::repo::list`].
//!
//! Uses `common::acquire(false)` so each test gets a throwaway database —
//! `ip_catalogue.ip` is globally unique, so fixture reuse across parallel
//! tests would collide on `10.0.0.*` addresses.
//!
//! ## What the binary proves
//!
//! 1. Every `(SortBy, SortDir)` pair round-trips 200 rows via repeated
//!    `next_cursor` walks; the concatenated page stream matches a
//!    Rust-side reference sort with `NULLS LAST` + `id DESC` tiebreaker.
//! 2. A cursor minted for one sort column is silently discarded when the
//!    next request uses a different `(sort, dir)` — the server returns a
//!    fresh first page rather than leaking into the NULLS-LAST tail.
//! 3. With duplicate sort-column clusters, no row is ever visited twice
//!    across a full walk — `id DESC` is a stable tiebreaker.
//! 4. An empty DB returns `(entries = [], total = 0, next_cursor = None)`.
//! 5. `next_cursor` is `None` when the SQL page didn't fill.

mod common;

use meshmon_service::catalogue::{
    dto::{SortBy, SortDir},
    model::{CatalogueEntry, CatalogueSource, EnrichmentStatus},
    repo,
    sort::Cursor,
};
use sqlx::PgPool;
use std::cmp::Ordering;
use std::net::IpAddr;

// --- Fixture ---------------------------------------------------------------

/// Total seeded rows.
const FIXTURE_ROWS: usize = 200;

/// Seed 200 rows with deliberate NULL bands and duplicate clusters per
/// nullable sort column. Returns the materialised entries so the tests
/// can run a Rust-side reference sort against them.
///
/// Layout (row index `i` in `0..200`):
///
/// - `ip`           = `10.0.0.i` (unique; `i < 200` so we stay inside /24).
/// - `city`         = NULL for `i < 25`; else `cities[i % 5]` (5 values, dozens of duplicates).
/// - `country_code` = NULL for `25 <= i < 50`; else `countries[i % 4]`.
/// - `asn`          = NULL for `50 <= i < 75`; else `64500 + (i % 10)`.
/// - `display_name` = NULL for `75 <= i < 100`; else `"host-<cluster>"` with `cluster = i / 20`.
/// - `network_op`   = NULL for `100 <= i < 125`; else `ops[i % 3]`.
/// - `website`      = NULL for `125 <= i < 150`; else grouped by `i / 40`.
/// - `lat` / `lng`  = NULL for `150 <= i < 200`; else `(i - 100, i - 100)`.
/// - `enrichment_status` = cycle `Pending` / `Enriched` / `Failed`.
///
/// `created_at` uses Postgres' `NOW()` default — rows share millisecond
/// clusters in practice, so the `id DESC` tiebreaker is what keeps the
/// `CreatedAt` sort deterministic.
async fn seed_paging_fixture(pool: &PgPool) -> Vec<CatalogueEntry> {
    let cities = ["Berlin", "Paris", "Tokyo", "Austin", "Lagos"];
    let countries = ["US", "DE", "FR", "JP"];
    let ops = ["OpA", "OpB", "OpC"];

    // One batch insert_many for the IPs, then per-row patch + direct SQL
    // for `enrichment_status` (which `patch` doesn't touch).
    let ips: Vec<IpAddr> = (0..FIXTURE_ROWS)
        .map(|i| format!("10.0.0.{}", i).parse().unwrap())
        .collect();
    let out = repo::insert_many(pool, &ips, CatalogueSource::Operator, None)
        .await
        .expect("insert_many");
    assert_eq!(out.created.len(), FIXTURE_ROWS, "all rows must be fresh");

    // Re-fetch the inserted rows in `ip` order so `i` maps 1-1 to the
    // row that owns `10.0.0.i`. `insert_many`'s returned order isn't a
    // guaranteed contract.
    let mut by_ip: std::collections::HashMap<IpAddr, _> =
        out.created.into_iter().map(|e| (e.ip, e)).collect();

    for i in 0..FIXTURE_ROWS {
        let ip: IpAddr = format!("10.0.0.{}", i).parse().unwrap();
        let row = by_ip.remove(&ip).expect("ip present after insert");
        let id = row.id;

        let city = (i >= 25).then(|| cities[i % 5].to_string());
        let country = (!(25..50).contains(&i)).then(|| countries[i % 4].to_string());
        let asn = (!(50..75).contains(&i)).then(|| 64500_i32 + (i as i32 % 10));
        let display_name = (!(75..100).contains(&i)).then(|| format!("host-{}", i / 20));
        let network_operator = (!(100..125).contains(&i)).then(|| ops[i % 3].to_string());
        let website =
            (!(125..150).contains(&i)).then(|| format!("https://e{}.example.com", i / 40));
        let has_coords = i < 150;
        let latitude = has_coords.then_some(i as f64 - 100.0);
        let longitude = has_coords.then_some(i as f64 - 100.0);

        repo::patch(
            pool,
            id,
            repo::PatchSet {
                city: city.map(Some),
                country_code: country.map(Some),
                asn: asn.map(Some),
                display_name: display_name.map(Some),
                network_operator: network_operator.map(Some),
                website: website.map(Some),
                latitude: latitude.map(Some),
                longitude: longitude.map(Some),
                ..Default::default()
            },
        )
        .await
        .expect("patch");

        let status = match i % 3 {
            0 => "pending",
            1 => "enriched",
            _ => "failed",
        };
        sqlx::query(&format!(
            "UPDATE ip_catalogue SET enrichment_status = '{}'::enrichment_status \
             WHERE id = $1",
            status
        ))
        .bind(id)
        .execute(pool)
        .await
        .expect("set enrichment_status");
    }

    // Now fetch every row back in a single query so the test has the
    // authoritative post-fixture state (including each row's actual `id`
    // and `created_at`).
    let rows: Vec<CatalogueEntry> = repo::list(
        pool,
        repo::ListFilter {
            limit: FIXTURE_ROWS as i64 + 10,
            ..Default::default()
        },
    )
    .await
    .expect("initial fetch")
    .0;
    assert_eq!(rows.len(), FIXTURE_ROWS, "fetched all seeded rows");
    rows
}

// --- Helpers ---------------------------------------------------------------

/// Walk every page of `filter` via repeated `next_cursor` hops until the
/// server returns `None`. Returns the concatenated rows + the `total`
/// reported by the first page.
async fn list_all_pages(pool: &PgPool, mut filter: repo::ListFilter) -> (Vec<CatalogueEntry>, i64) {
    let mut acc: Vec<CatalogueEntry> = Vec::new();
    let (first_rows, first_total, mut next) = repo::list(pool, clone_filter(&filter))
        .await
        .expect("first page");
    acc.extend(first_rows);
    // Safety valve: an unbounded loop against a buggy server would hang
    // the test suite instead of failing it. Cap to enough iterations for
    // the 200-row fixture at any reasonable page size.
    for _ in 0..1_000 {
        let Some(cursor) = next.take() else { break };
        filter.after = Some(cursor);
        let (rows, _total, cont) = repo::list(pool, clone_filter(&filter))
            .await
            .expect("next page");
        acc.extend(rows);
        next = cont;
    }
    assert!(next.is_none(), "paging loop exceeded safety cap");
    (acc, first_total)
}

/// `ListFilter` isn't `Clone`; hand-build a fresh copy for each call
/// since `repo::list` consumes its argument.
fn clone_filter(f: &repo::ListFilter) -> repo::ListFilter {
    repo::ListFilter {
        country_code: f.country_code.clone(),
        asn: f.asn.clone(),
        network: f.network.clone(),
        ip_prefix: f.ip_prefix.clone(),
        name: f.name.clone(),
        bounding_box: f.bounding_box,
        city: f.city.clone(),
        shapes: f.shapes.iter().map(clone_polygon).collect(),
        sort: f.sort,
        sort_dir: f.sort_dir,
        after: f.after.clone(),
        limit: f.limit,
    }
}

fn clone_polygon(
    p: &meshmon_service::catalogue::dto::Polygon,
) -> meshmon_service::catalogue::dto::Polygon {
    meshmon_service::catalogue::dto::Polygon(p.0.clone())
}

/// `NULLS LAST` comparator: `None` always sorts *after* `Some`,
/// regardless of direction.
fn nulls_last_cmp<T: Ord>(a: &Option<T>, b: &Option<T>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Float variant — the fixture's lat/lng pairs are always `i - 100`,
/// distinct integers, so `partial_cmp` never returns `None` here.
fn cmp_by_location(a: &CatalogueEntry, b: &CatalogueEntry) -> Ordering {
    let ac = a.latitude.is_some() && a.longitude.is_some();
    let bc = b.latitude.is_some() && b.longitude.is_some();
    ac.cmp(&bc)
}

/// Rendered string for `enrichment_status`, matching the repo layer's
/// `::text` cast so the Rust-side reference sort lines up with
/// Postgres' alphabetical ordering (`enriched` < `failed` < `pending`).
fn status_text(e: EnrichmentStatus) -> &'static str {
    match e {
        EnrichmentStatus::Pending => "pending",
        EnrichmentStatus::Enriched => "enriched",
        EnrichmentStatus::Failed => "failed",
    }
}

/// Sort `rows` the same way the SQL layer should: primary column by
/// `(sort, dir)` with `NULLS LAST`, tiebreaker `id DESC`.
fn reference_sort(
    mut rows: Vec<CatalogueEntry>,
    sort: SortBy,
    dir: SortDir,
) -> Vec<CatalogueEntry> {
    rows.sort_by(|a, b| {
        let primary = match sort {
            SortBy::CreatedAt => a.created_at.cmp(&b.created_at),
            SortBy::Ip => a.ip.cmp(&b.ip),
            SortBy::DisplayName => nulls_last_cmp(&a.display_name, &b.display_name),
            SortBy::City => nulls_last_cmp(&a.city, &b.city),
            SortBy::CountryCode => nulls_last_cmp(&a.country_code, &b.country_code),
            SortBy::Asn => nulls_last_cmp(&a.asn, &b.asn),
            SortBy::NetworkOperator => nulls_last_cmp(&a.network_operator, &b.network_operator),
            SortBy::EnrichmentStatus => {
                status_text(a.enrichment_status).cmp(status_text(b.enrichment_status))
            }
            SortBy::Website => nulls_last_cmp(&a.website, &b.website),
            SortBy::Location => cmp_by_location(a, b),
        };
        // Apply direction to the primary column, but keep `NULLS LAST`
        // invariant: for nullable columns `nulls_last_cmp` already
        // placed `None` at `Greater`, so a naive `.reverse()` would flip
        // NULLs to the front on Desc. Pull NULL detection out and handle
        // it independently.
        let primary = match dir {
            SortDir::Asc => primary,
            SortDir::Desc => {
                // Only reverse when both sides are non-null; otherwise
                // keep the `NULLS LAST` decision from `nulls_last_cmp`
                // (or the always-non-null path for NOT NULL columns).
                let a_null = is_null_for_sort(a, sort);
                let b_null = is_null_for_sort(b, sort);
                match (a_null, b_null) {
                    (false, false) => primary.reverse(),
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (true, true) => Ordering::Equal,
                }
            }
        };
        // `id DESC` tiebreaker — invariant across both directions.
        primary.then_with(|| b.id.cmp(&a.id))
    });
    rows
}

/// Is `row`'s sort column NULL? Used by `reference_sort` to preserve
/// `NULLS LAST` across both directions. NOT NULL columns always return
/// `false`.
fn is_null_for_sort(row: &CatalogueEntry, sort: SortBy) -> bool {
    match sort {
        SortBy::CreatedAt | SortBy::Ip | SortBy::EnrichmentStatus | SortBy::Location => false,
        SortBy::DisplayName => row.display_name.is_none(),
        SortBy::City => row.city.is_none(),
        SortBy::CountryCode => row.country_code.is_none(),
        SortBy::Asn => row.asn.is_none(),
        SortBy::NetworkOperator => row.network_operator.is_none(),
        SortBy::Website => row.website.is_none(),
    }
}

// --- Round-trip tests ------------------------------------------------------

/// Run the full-set round-trip assertion for a single `(sort, dir)` pair.
///
/// Seed, paginate via `next_cursor` until exhausted, assert both count
/// and order line up with `reference_sort`.
async fn run_round_trip(sort: SortBy, dir: SortDir) {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    let fixture = seed_paging_fixture(&db.pool).await;

    let filter = repo::ListFilter {
        sort,
        sort_dir: dir,
        limit: 50,
        ..Default::default()
    };
    let (paged, total) = list_all_pages(&db.pool, filter).await;

    assert_eq!(
        paged.len(),
        FIXTURE_ROWS,
        "sort={:?} dir={:?}: page count must equal fixture size",
        sort,
        dir,
    );
    assert_eq!(total, FIXTURE_ROWS as i64, "total must equal fixture size");

    let reference = reference_sort(fixture, sort, dir);
    let paged_ids: Vec<_> = paged.iter().map(|r| r.id).collect();
    let ref_ids: Vec<_> = reference.iter().map(|r| r.id).collect();
    assert_eq!(
        paged_ids, ref_ids,
        "sort={:?} dir={:?}: paged id order must match reference sort",
        sort, dir,
    );

    db.close().await;
}

/// Expand one `#[tokio::test]` per sort variant. Each test lands as a
/// distinct cargo-test name so failures point at the failing pair.
macro_rules! round_trip_test {
    ($name:ident, $sort:expr, $dir:expr) => {
        #[tokio::test]
        async fn $name() {
            run_round_trip($sort, $dir).await;
        }
    };
}

round_trip_test!(round_trip_created_at_asc, SortBy::CreatedAt, SortDir::Asc);
round_trip_test!(round_trip_created_at_desc, SortBy::CreatedAt, SortDir::Desc);
round_trip_test!(round_trip_ip_asc, SortBy::Ip, SortDir::Asc);
round_trip_test!(round_trip_ip_desc, SortBy::Ip, SortDir::Desc);
round_trip_test!(
    round_trip_display_name_asc,
    SortBy::DisplayName,
    SortDir::Asc
);
round_trip_test!(
    round_trip_display_name_desc,
    SortBy::DisplayName,
    SortDir::Desc
);
round_trip_test!(round_trip_city_asc, SortBy::City, SortDir::Asc);
round_trip_test!(round_trip_city_desc, SortBy::City, SortDir::Desc);
round_trip_test!(
    round_trip_country_code_asc,
    SortBy::CountryCode,
    SortDir::Asc
);
round_trip_test!(
    round_trip_country_code_desc,
    SortBy::CountryCode,
    SortDir::Desc
);
round_trip_test!(round_trip_asn_asc, SortBy::Asn, SortDir::Asc);
round_trip_test!(round_trip_asn_desc, SortBy::Asn, SortDir::Desc);
round_trip_test!(
    round_trip_network_operator_asc,
    SortBy::NetworkOperator,
    SortDir::Asc
);
round_trip_test!(
    round_trip_network_operator_desc,
    SortBy::NetworkOperator,
    SortDir::Desc
);
round_trip_test!(
    round_trip_enrichment_status_asc,
    SortBy::EnrichmentStatus,
    SortDir::Asc
);
round_trip_test!(
    round_trip_enrichment_status_desc,
    SortBy::EnrichmentStatus,
    SortDir::Desc
);
round_trip_test!(round_trip_website_asc, SortBy::Website, SortDir::Asc);
round_trip_test!(round_trip_website_desc, SortBy::Website, SortDir::Desc);
round_trip_test!(round_trip_location_asc, SortBy::Location, SortDir::Asc);
round_trip_test!(round_trip_location_desc, SortBy::Location, SortDir::Desc);

// --- Cursor-mismatch test --------------------------------------------------

/// A cursor minted for one `(sort, dir)` pair is silently dropped when
/// the next request uses a different pair. The response must match a
/// fresh first-page `(sort, dir)` call, proving the server didn't leak
/// into the NULLS LAST tail or otherwise trust the stale cursor.
#[tokio::test]
async fn cursor_with_mismatched_sort_returns_fresh_page() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    let _fixture = seed_paging_fixture(&db.pool).await;

    let (_rows_city, _total_city, city_cursor) = repo::list(
        &db.pool,
        repo::ListFilter {
            sort: SortBy::City,
            sort_dir: SortDir::Asc,
            limit: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let city_cursor = city_cursor.expect("non-final page must mint a cursor");

    // Request page 2 of (Asn, Desc) but pass the stale City cursor.
    let (actual_rows, _actual_total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            sort: SortBy::Asn,
            sort_dir: SortDir::Desc,
            limit: 25,
            after: Some(city_cursor),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Reference: a fresh `(Asn, Desc)` request with no cursor.
    let (fresh_rows, _fresh_total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            sort: SortBy::Asn,
            sort_dir: SortDir::Desc,
            limit: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let actual_ids: Vec<_> = actual_rows.iter().map(|r| r.id).collect();
    let fresh_ids: Vec<_> = fresh_rows.iter().map(|r| r.id).collect();
    assert_eq!(
        actual_ids, fresh_ids,
        "stale cursor with mismatched (sort, dir) must collapse to a fresh page",
    );

    db.close().await;
}

// --- Tiebreaker stability --------------------------------------------------

/// Walk pages of 3 at a time over the `City` sort (which has the biggest
/// duplicate clusters in the fixture) and assert every id shows up
/// exactly once — `id DESC` is a stable tiebreaker under the smallest
/// page size the cursor codec will exercise.
#[tokio::test]
async fn tiebreaker_is_stable_under_duplicates() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    let _fixture = seed_paging_fixture(&db.pool).await;

    let (paged, _total) = list_all_pages(
        &db.pool,
        repo::ListFilter {
            sort: SortBy::City,
            sort_dir: SortDir::Asc,
            limit: 3,
            ..Default::default()
        },
    )
    .await;

    assert_eq!(paged.len(), FIXTURE_ROWS, "every row must be visited");
    let mut seen = std::collections::HashSet::new();
    for row in &paged {
        assert!(
            seen.insert(row.id),
            "row {} visited twice under page-size-3 walk",
            row.id,
        );
    }

    db.close().await;
}

// --- Empty + final-page edges ---------------------------------------------

/// With no fixture at all, the first page must report zero total, zero
/// entries, and no cursor — no "phantom cursor" for a server that
/// accidentally mints one on an empty result.
#[tokio::test]
async fn empty_database_returns_none_cursor() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let (rows, total, next) = repo::list(
        &db.pool,
        repo::ListFilter {
            sort: SortBy::CreatedAt,
            sort_dir: SortDir::Desc,
            limit: 50,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(rows.is_empty(), "no rows in an empty DB");
    assert_eq!(total, 0);
    assert!(next.is_none(), "empty page must not mint a cursor");

    db.close().await;
}

/// Seed 200 rows and request a single page of 250. The SQL page doesn't
/// fill `limit`, so `next_cursor` must be `None` even though there's
/// technically "no more data" (a page-filling 250 would mint a cursor).
#[tokio::test]
async fn next_cursor_none_on_final_page() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    let _fixture = seed_paging_fixture(&db.pool).await;

    let (rows, total, next) = repo::list(
        &db.pool,
        repo::ListFilter {
            sort: SortBy::CreatedAt,
            sort_dir: SortDir::Desc,
            limit: 250,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), FIXTURE_ROWS);
    assert_eq!(total, FIXTURE_ROWS as i64);
    assert!(
        next.is_none(),
        "under-filled SQL page must not mint a cursor",
    );

    db.close().await;
}

// Suppress unused-warning for `Cursor` on binaries that only reference
// it indirectly through `ListFilter::after`.
#[allow(dead_code)]
fn _force_cursor_in_scope(_c: &Cursor) {}
