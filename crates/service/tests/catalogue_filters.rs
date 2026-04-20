//! Integration tests for the new `city` and `shapes` filters on
//! [`meshmon_service::catalogue::repo::list`].
//!
//! Uses `common::acquire(false)` — the fixtures here share the same
//! unique-on-`ip` constraint as the paging tests, so parallel runs
//! cannot share a database.
//!
//! ## What the binary proves
//!
//! - `city`: single-value and multi-value ANY semantics, empty-vec =
//!   no-filter, NULL rows excluded whenever the filter is non-empty.
//! - `shapes`: point-in-polygon with one or more rings (OR across
//!   rings), composition with other filters (`country_code`), the
//!   server-side bbox-only `total` approximation, empty-vec =
//!   no-filter, rows missing lat/lng dropped.
//! - Three-way composition across `city` + `shapes` + `country_code`.

mod common;

use meshmon_service::catalogue::{
    dto::{Polygon, SortBy, SortDir},
    model::CatalogueSource,
    repo,
};
use sqlx::PgPool;
use std::net::IpAddr;
use uuid::Uuid;

// --- Fixture helpers -------------------------------------------------------

/// Deterministic row-shaping helper.
///
/// Inserts one row at `10.0.0.<ip_last_octet>` via `insert_many`, then
/// `patch`es the enrichable columns. Returns the freshly-inserted id
/// so tests can cross-check per-row expectations without re-fetching.
#[allow(clippy::too_many_arguments)]
async fn seed_row(
    pool: &PgPool,
    ip_last_octet: u8,
    city: Option<&str>,
    country: Option<&str>,
    asn: Option<i32>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    display_name: Option<&str>,
) -> Uuid {
    let ip: IpAddr = format!("10.0.0.{}", ip_last_octet).parse().unwrap();
    let out = repo::insert_many(pool, &[ip], CatalogueSource::Operator, None)
        .await
        .expect("insert_many");
    let id = out
        .created
        .first()
        .unwrap_or_else(|| {
            panic!(
                "seed_row: ip {} unexpectedly already present in fixture",
                ip
            )
        })
        .id;

    repo::patch(
        pool,
        id,
        repo::PatchSet {
            city: city.map(|s| Some(s.to_string())),
            country_code: country.map(|s| Some(s.to_string())),
            asn: asn.map(Some),
            latitude: latitude.map(Some),
            longitude: longitude.map(Some),
            display_name: display_name.map(|s| Some(s.to_string())),
            ..Default::default()
        },
    )
    .await
    .expect("patch");
    id
}

/// Build a fresh `ListFilter` with everything at defaults — tests then
/// override only the fields they care about. `repo::list` consumes its
/// argument; callers re-invoke this per request.
fn base_filter() -> repo::ListFilter {
    repo::ListFilter {
        sort: SortBy::CreatedAt,
        sort_dir: SortDir::Desc,
        limit: 500,
        ..Default::default()
    }
}

/// Closed square ring `[lng, lat]` covering `[(min_lng, min_lat), (max_lng, max_lat)]`.
fn closed_square(min_lng: f64, min_lat: f64, max_lng: f64, max_lat: f64) -> Polygon {
    Polygon(vec![
        [min_lng, min_lat],
        [max_lng, min_lat],
        [max_lng, max_lat],
        [min_lng, max_lat],
        [min_lng, min_lat],
    ])
}

// --- Shared city fixture --------------------------------------------------

/// Seed 20 rows with `city` populated across 4 cities (5 rows each).
/// No NULL cities — the `with_nulls` test layers those on top.
async fn seed_cities_20(pool: &PgPool) {
    let cities = ["Berlin", "Paris", "Tokyo", "Austin"];
    for i in 0..20_u8 {
        let city = cities[i as usize / 5];
        seed_row(pool, i, Some(city), None, None, None, None, None).await;
    }
}

// --- Shared shapes fixture ------------------------------------------------

/// Seed 10 rows with known coordinates `(i, i)` for `i in 10..20`.
/// Alternates `country_code` between `US` and `DE` in the `i % 2`
/// pattern so shape-composition tests can intersect with country.
async fn seed_shapes_10(pool: &PgPool) {
    for i in 10..20_u8 {
        let country = if i % 2 == 0 { "US" } else { "DE" };
        seed_row(
            pool,
            i,
            None,
            Some(country),
            None,
            Some(i as f64),
            Some(i as f64),
            None,
        )
        .await;
    }
}

// --- City-filter tests -----------------------------------------------------

#[tokio::test]
async fn city_single_value_returns_exact_matches() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_cities_20(&db.pool).await;

    let (rows, total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            city: vec!["Berlin".into()],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), 5, "exactly 5 Berlin rows in fixture");
    assert_eq!(total, 5, "total must match the filtered count (no shapes)");
    for row in &rows {
        assert_eq!(row.city.as_deref(), Some("Berlin"));
    }

    db.close().await;
}

#[tokio::test]
async fn city_multi_value_is_any_semantics() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_cities_20(&db.pool).await;

    let (rows, total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            city: vec!["Berlin".into(), "Paris".into()],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), 10, "5 Berlin + 5 Paris = 10");
    assert_eq!(total, 10);
    for row in &rows {
        assert!(
            matches!(row.city.as_deref(), Some("Berlin") | Some("Paris")),
            "row city {:?} not in the filter set",
            row.city,
        );
    }

    db.close().await;
}

#[tokio::test]
async fn city_empty_vec_is_no_filter() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_cities_20(&db.pool).await;

    let (rows, total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            city: vec![],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), 20, "empty vec must be a no-op filter");
    assert_eq!(total, 20);

    db.close().await;
}

#[tokio::test]
async fn city_with_nulls_excludes_null_rows() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_cities_20(&db.pool).await;
    // Add 5 rows with NULL city. ip_last_octet must not collide with
    // the 0..20 range used by `seed_cities_20`.
    for i in 100..105_u8 {
        seed_row(&db.pool, i, None, Some("US"), None, None, None, None).await;
    }

    let (rows, total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            city: vec!["Berlin".into()],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), 5, "NULL-city rows must not match city filter");
    assert_eq!(total, 5);
    for row in &rows {
        assert_eq!(row.city.as_deref(), Some("Berlin"));
    }

    db.close().await;
}

// --- Shapes-filter tests ---------------------------------------------------

#[tokio::test]
async fn single_polygon_returns_enclosed_rows() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_shapes_10(&db.pool).await;

    // Square covering `(9, 9)` to `(12.5, 12.5)` — should enclose the
    // rows at `(10,10)`, `(11,11)`, `(12,12)`. Remember: the wire form
    // is `[lng, lat]`.
    let polygon = closed_square(9.0, 9.0, 12.5, 12.5);

    let (rows, total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            shapes: vec![polygon],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    let lat_lngs: Vec<(f64, f64)> = rows
        .iter()
        .map(|r| (r.latitude.unwrap(), r.longitude.unwrap()))
        .collect();
    assert_eq!(
        rows.len(),
        3,
        "expected 3 enclosed rows, got {:?}",
        lat_lngs,
    );
    for (lat, lng) in &lat_lngs {
        assert!(
            (10.0..=12.0).contains(lat) && (10.0..=12.0).contains(lng),
            "row ({}, {}) outside expected range",
            lat,
            lng,
        );
    }
    // SQL bbox count includes anything inside `[9..12.5]` on both axes
    // — the fixture puts 3 such rows there. Here `total == entries.len()`
    // because the polygon is an axis-aligned square identical to the
    // bbox, so no PIP drop-off occurs.
    assert_eq!(total, 3);

    db.close().await;
}

#[tokio::test]
async fn multi_polygon_is_or_semantics() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_shapes_10(&db.pool).await;

    // Two disjoint squares.
    //   A: (10, 10) / (11, 11)  — both inside.
    //   B: (15, 15) / (16, 16)  — both inside.
    // The union bbox still spans `[9..17]` on both axes, so the SQL
    // pre-filter admits every row in between (12, 13, 14) — PIP drops
    // those from `rows`.
    let a = closed_square(9.5, 9.5, 11.5, 11.5);
    let b = closed_square(14.5, 14.5, 16.5, 16.5);

    let (rows, _total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            shapes: vec![a, b],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    let mut ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), rows.len(), "polygon union must not duplicate");

    let lat_lngs: Vec<(f64, f64)> = rows
        .iter()
        .map(|r| (r.latitude.unwrap(), r.longitude.unwrap()))
        .collect();
    assert_eq!(rows.len(), 4, "expected 4 rows, got {:?}", lat_lngs);
    for (lat, _) in &lat_lngs {
        assert!(
            (10.0..=11.0).contains(lat) || (15.0..=16.0).contains(lat),
            "row with lat {} escaped both polygons",
            lat,
        );
    }

    db.close().await;
}

#[tokio::test]
async fn shapes_compose_with_country_filter() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_shapes_10(&db.pool).await;

    // Square enclosing `(10,10)`, `(11,11)`, `(12,12)`.
    // Of those, `10` and `12` were seeded with country_code = "US"
    // (`i % 2 == 0`), `11` with "DE".
    let polygon = closed_square(9.0, 9.0, 12.5, 12.5);

    let (rows, _total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            shapes: vec![polygon],
            country_code: vec!["US".into()],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), 2, "only US rows inside the polygon");
    for row in &rows {
        assert_eq!(row.country_code.as_deref(), Some("US"));
    }

    db.close().await;
}

#[tokio::test]
async fn total_approximation_when_shapes_non_empty() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_shapes_10(&db.pool).await;

    // A "diagonal-shaped" polygon that the SQL bbox sees as
    // `[9..20]` on both axes (so 10 rows match in SQL) but whose
    // actual interior is a narrow triangle covering only `(10,10)`,
    // `(11,11)`, `(12,12)`. The PIP pass drops the rest.
    //
    // Triangle vertices `(lng, lat)`:
    //   (9, 9), (12.5, 9), (12.5, 12.5)
    // yields a union bbox `[9..12.5]` on both axes — only 3 rows. To
    // hit "SQL bbox says 10 but PIP says 3" we need a polygon whose
    // vertex bbox touches every fixture row — use the outer corners.
    //
    // The polygon is a skinny diagonal band: `(9, 9) - (20, 20) -
    // (20, 19) - (9, 8)`. Vertex bbox = `[8..20]` on lat, `[9..20]` on
    // lng — every seeded row (at `(i, i)`, i = 10..20) is inside the
    // bbox. PIP keeps only points inside the band, which at 1-unit
    // tolerance includes `(10..20, 10..20)` near the `y = x` diagonal
    // — in practice all 10 fixture rows.
    //
    // Switch to a triangle whose bbox still covers every fixture row
    // but whose interior only touches a handful: vertices
    //   (9, 9), (20, 9), (9, 20)
    // — bbox `[9..20]` both axes (catches all 10 fixture rows) but
    // the triangle's `x + y <= 29` body keeps `(10,10)..(14,14)` and
    // drops `(15,15)..(19,19)`. Still multiple rows, but enough to
    // hit "SQL total > entries.len()".
    let polygon = Polygon(vec![[9.0, 9.0], [20.0, 9.0], [9.0, 20.0], [9.0, 9.0]]);

    let (rows, total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            shapes: vec![polygon],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    // Expected PIP survivors: rows where `lat + lng <= 29` — i.e.
    // `(10,10), (11,11), (12,12), (13,13), (14,14)`. Boundary at
    // `(14.5, 14.5)` sits exactly on the hypotenuse — the `(14,14)`
    // row is strictly inside. `(15,15)` has `x+y = 30 > 29`, dropped.
    assert_eq!(
        rows.len(),
        5,
        "PIP should drop the four rows outside the triangle",
    );
    // `total` is bbox-only: the SQL pre-filter admits every fixture
    // row inside `[9..20]` × `[9..20]` — all 10.
    assert_eq!(
        total, 10,
        "total must be the bbox-level approximation (not the PIP count)",
    );
    assert!(
        total >= rows.len() as i64,
        "total must be an upper bound on the shown row count",
    );

    db.close().await;
}

#[tokio::test]
async fn empty_shapes_vec_is_no_filter() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_shapes_10(&db.pool).await;

    let (rows, total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            shapes: vec![],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), 10, "empty shapes vec must be a no-op");
    assert_eq!(total, 10);

    db.close().await;
}

#[tokio::test]
async fn shape_with_rows_missing_coords() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_shapes_10(&db.pool).await;
    // Add 3 rows with NULL lat/lng. Must not leak into the shapes
    // match regardless of how wide the polygon is.
    for i in 100..103_u8 {
        seed_row(&db.pool, i, None, Some("US"), None, None, None, None).await;
    }

    // A polygon whose bbox spans every conceivable lat/lng — if the
    // implementation didn't drop NULL-coord rows, they'd sneak in.
    let polygon = closed_square(-180.0, -90.0, 180.0, 90.0);

    let (rows, _total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            shapes: vec![polygon],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    assert_eq!(
        rows.len(),
        10,
        "only rows with coordinates survive the shapes post-filter",
    );
    for row in &rows {
        assert!(row.latitude.is_some());
        assert!(row.longitude.is_some());
    }

    db.close().await;
}

// --- Three-way composition -------------------------------------------------

#[tokio::test]
async fn city_and_shapes_and_country_compose() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    // Custom fixture — the seeded coords, cities, and countries are
    // chosen so exactly one row survives the three-way AND.
    //
    // Row layout (ip_last_octet, city, country, lat/lng):
    //   0: Berlin,  DE, (10, 10)   — inside polygon + country DE + city Berlin ✔
    //   1: Berlin,  DE, (50, 50)   — country + city OK, outside polygon
    //   2: Berlin,  US, (10, 10)   — inside polygon + city OK, country mismatch
    //   3: Tokyo,   DE, (10, 10)   — inside polygon + country OK, city mismatch
    //   4: Paris,   DE, (11, 11)   — inside polygon + country OK, city mismatch
    for (ip, city, country, (lat, lng)) in [
        (0_u8, "Berlin", "DE", (10.0, 10.0)),
        (1_u8, "Berlin", "DE", (50.0, 50.0)),
        (2_u8, "Berlin", "US", (10.0, 10.0)),
        (3_u8, "Tokyo", "DE", (10.0, 10.0)),
        (4_u8, "Paris", "DE", (11.0, 11.0)),
    ] {
        seed_row(
            &db.pool,
            ip,
            Some(city),
            Some(country),
            None,
            Some(lat),
            Some(lng),
            None,
        )
        .await;
    }

    let polygon = closed_square(9.0, 9.0, 12.5, 12.5);

    let (rows, _total, _) = repo::list(
        &db.pool,
        repo::ListFilter {
            city: vec!["Berlin".into()],
            country_code: vec!["DE".into()],
            shapes: vec![polygon],
            ..base_filter()
        },
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), 1, "only row 0 satisfies all three filters");
    let hit = &rows[0];
    assert_eq!(hit.city.as_deref(), Some("Berlin"));
    assert_eq!(hit.country_code.as_deref(), Some("DE"));
    assert_eq!(hit.latitude, Some(10.0));
    assert_eq!(hit.longitude, Some(10.0));

    db.close().await;
}
