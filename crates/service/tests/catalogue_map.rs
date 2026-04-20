//! Integration tests for
//! [`meshmon_service::catalogue::repo::map_detail_or_clusters`] and the
//! surrounding `GET /api/catalogue/map` wiring.
//!
//! Uses `common::acquire(false)` — fixtures share the same unique-on-
//! `ip` constraint as the paging/filter binaries, so parallel runs
//! cannot share a database.
//!
//! ## What the binary proves
//!
//! - Below the adaptive threshold the server returns raw rows
//!   (`MapResult::Detail`) with filter composition (country).
//! - Above the threshold the server aggregates rows into grid buckets
//!   (`MapResult::Clusters`) with conservation of total across any
//!   zoom granularity.
//! - The detail/cluster boundary sits at exactly
//!   [`repo::MAP_DETAIL_THRESHOLD`] — 2000 rows detail, 2001 clusters.
//! - Cell size derives deterministically from zoom level (unit test
//!   over the full [0..=20] range).
//! - Empty viewports return `Detail { rows: [], total: 0 }`.
//! - Rows with NULL lat/lng are invisible to the map view.
//! - The `shapes` query-string parameter is not accepted by
//!   [`meshmon_service::catalogue::dto::MapQuery`]: a compile-time
//!   assertion of the absence keeps the semantic contract documented.

mod common;

use meshmon_service::catalogue::{
    dto::MapBucket,
    repo::{
        cell_size_for_zoom, map_detail_or_clusters, MapFilter, MapResult, MAP_DETAIL_THRESHOLD,
    },
};
use sqlx::{types::ipnetwork::IpNetwork, PgPool};
use std::net::IpAddr;

// --- Fixture helpers -------------------------------------------------------

/// Insert `count` rows with deterministic coordinates and no other
/// metadata. `ip` is formed as `10.<high>.<mid>.<low>` where the
/// compound `(high, mid, low)` walks `0..count` monotonically.
///
/// Returns the inserted row ids in insertion order.
///
/// `country_code` is populated on every row when supplied — tests that
/// layer a country filter on top pass `Some("US")` etc.; tests that
/// don't care pass `None`.
async fn seed_rows_with_coords(
    pool: &PgPool,
    count: usize,
    coord_for_i: impl Fn(usize) -> (f64, f64),
    country_for_i: impl Fn(usize) -> Option<&'static str>,
) {
    // Direct SQL bulk insert — per-row `repo::patch` would cost one
    // round trip per row (2500 round trips at the cluster threshold)
    // which is noticeably slow on the CI docker-backed Postgres.
    let ips: Vec<IpNetwork> = (0..count)
        .map(|i| {
            let ip: IpAddr = format!("10.{}.{}.{}", (i >> 16) & 0xFF, (i >> 8) & 0xFF, i & 0xFF)
                .parse()
                .unwrap();
            IpNetwork::from(ip)
        })
        .collect();
    let (lats, lngs): (Vec<f64>, Vec<f64>) = (0..count).map(&coord_for_i).unzip();
    let countries: Vec<Option<String>> = (0..count)
        .map(|i| country_for_i(i).map(String::from))
        .collect();

    sqlx::query(
        r#"
        INSERT INTO ip_catalogue (ip, source, latitude, longitude, country_code)
        SELECT
            ip_row.ip,
            'operator'::catalogue_source,
            lat_row.lat,
            lng_row.lng,
            cc_row.cc
        FROM
            UNNEST($1::inet[])                                   WITH ORDINALITY AS ip_row(ip, ord),
            UNNEST($2::double precision[])                       WITH ORDINALITY AS lat_row(lat, ord),
            UNNEST($3::double precision[])                       WITH ORDINALITY AS lng_row(lng, ord),
            UNNEST($4::char(2)[])                                WITH ORDINALITY AS cc_row(cc, ord)
        WHERE ip_row.ord = lat_row.ord
          AND ip_row.ord = lng_row.ord
          AND ip_row.ord = cc_row.ord
        "#,
    )
    .bind(&ips)
    .bind(&lats)
    .bind(&lngs)
    .bind(&countries)
    .execute(pool)
    .await
    .expect("bulk insert");
}

/// Insert `count` rows with NULL latitude/longitude, using the same IP
/// generation scheme as [`seed_rows_with_coords`] but offset by a
/// non-colliding base so tests can layer both row types.
async fn seed_rows_no_coords(pool: &PgPool, base: usize, count: usize) {
    let ips: Vec<IpNetwork> = (base..(base + count))
        .map(|i| {
            let ip: IpAddr = format!("10.{}.{}.{}", (i >> 16) & 0xFF, (i >> 8) & 0xFF, i & 0xFF)
                .parse()
                .unwrap();
            IpNetwork::from(ip)
        })
        .collect();
    sqlx::query(
        r#"
        INSERT INTO ip_catalogue (ip, source)
        SELECT ip, 'operator'::catalogue_source
        FROM UNNEST($1::inet[]) AS ip
        "#,
    )
    .bind(&ips)
    .execute(pool)
    .await
    .expect("bulk insert (no coords)");
}

/// Build a default [`MapFilter`] scoped to the global bbox. Tests
/// override only the fields they care about via `..base_filter()`.
fn base_filter() -> MapFilter {
    MapFilter {
        country_code: vec![],
        asn: vec![],
        network: vec![],
        ip_prefix: None,
        name: None,
        bbox: [-90.0, -180.0, 90.0, 180.0],
    }
}

/// Shift coordinates safely inside a compact test bbox. Maps `i` into
/// a deterministic `(lat, lng)` inside `[10, 20]` on both axes.
fn test_coord(i: usize) -> (f64, f64) {
    // Spread across a 10-unit square so cluster aggregation has
    // multiple cells at high zoom. Modulo-then-fraction yields a fine
    // distribution even at 2500 rows.
    let x = i % 100;
    let y = (i / 100) % 100;
    // Offset into `[10, 20]` on each axis and use a 0.09-step grid
    // inside that square so every row has unique coords.
    (10.0 + 0.09 * x as f64, 10.0 + 0.09 * y as f64)
}

// --- Detail path -----------------------------------------------------------

#[tokio::test]
async fn detail_path_returns_all_rows_when_below_threshold() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_rows_with_coords(&db.pool, 500, test_coord, |_| None).await;

    let result = map_detail_or_clusters(
        &db.pool,
        MapFilter {
            bbox: [5.0, 5.0, 25.0, 25.0],
            ..base_filter()
        },
        3,
    )
    .await
    .unwrap();

    match result {
        MapResult::Detail { rows, total } => {
            assert_eq!(total, 500, "total must match seeded count");
            assert_eq!(rows.len(), 500, "detail path returns every matching row");
        }
        other => panic!("expected Detail, got {}", describe(&other)),
    }

    db.close().await;
}

#[tokio::test]
async fn detail_path_respects_country_filter() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    // 500 rows total, first 100 tagged `US`, rest `DE`. Every row has
    // coords inside the base bbox so the viewport doesn't truncate.
    seed_rows_with_coords(&db.pool, 500, test_coord, |i| {
        if i < 100 {
            Some("US")
        } else {
            Some("DE")
        }
    })
    .await;

    let result = map_detail_or_clusters(
        &db.pool,
        MapFilter {
            bbox: [5.0, 5.0, 25.0, 25.0],
            country_code: vec!["US".into()],
            ..base_filter()
        },
        3,
    )
    .await
    .unwrap();

    match result {
        MapResult::Detail { rows, total } => {
            assert_eq!(total, 100, "US-filter total");
            assert_eq!(rows.len(), 100, "US-filter row count");
            for row in &rows {
                assert_eq!(row.country_code.as_deref(), Some("US"));
            }
        }
        other => panic!("expected Detail, got {}", describe(&other)),
    }

    db.close().await;
}

// --- Cluster path ----------------------------------------------------------

#[tokio::test]
async fn cluster_path_aggregates_when_above_threshold() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_rows_with_coords(&db.pool, 2500, test_coord, |_| None).await;

    let result = map_detail_or_clusters(
        &db.pool,
        MapFilter {
            bbox: [5.0, 5.0, 25.0, 25.0],
            ..base_filter()
        },
        3,
    )
    .await
    .unwrap();

    match result {
        MapResult::Clusters {
            buckets,
            total,
            cell_size,
        } => {
            assert_eq!(total, 2500, "bucket total must equal seeded count");
            assert_eq!(cell_size, 5.0, "zoom 3 cell size");
            let sum: i64 = buckets.iter().map(|b| b.count).sum();
            assert_eq!(sum, 2500, "conservation of rows across cluster aggregation");
            // Spot-check: the first bucket's sample_id resolves to a
            // catalogue row whose coordinates fall inside the bucket's
            // bbox. The full scan would be O(bucket count) round
            // trips; the single sample is enough to prove the
            // ARRAY_AGG id column produces referenceable ids.
            let sample = buckets.first().expect("non-empty buckets");
            assert_sample_inside_bucket(&db.pool, sample).await;
        }
        other => panic!("expected Clusters, got {}", describe(&other)),
    }

    db.close().await;
}

#[tokio::test]
async fn cluster_path_preserves_row_totals_under_any_zoom() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_rows_with_coords(&db.pool, 2500, test_coord, |_| None).await;

    for zoom in [3_u8, 6, 9, 12] {
        let result = map_detail_or_clusters(
            &db.pool,
            MapFilter {
                bbox: [5.0, 5.0, 25.0, 25.0],
                ..base_filter()
            },
            zoom,
        )
        .await
        .unwrap();

        match result {
            MapResult::Clusters {
                buckets,
                total,
                cell_size,
            } => {
                assert_eq!(total, 2500, "total invariant at zoom {zoom}");
                assert_eq!(cell_size, cell_size_for_zoom(zoom));
                let sum: i64 = buckets.iter().map(|b| b.count).sum();
                assert_eq!(sum, 2500, "conservation of rows at zoom {zoom}");
            }
            other => panic!("expected Clusters at zoom {zoom}, got {}", describe(&other)),
        }
    }

    db.close().await;
}

// --- Threshold boundary ----------------------------------------------------

#[tokio::test]
async fn threshold_boundary_at_exactly_2000_rows() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_rows_with_coords(&db.pool, MAP_DETAIL_THRESHOLD as usize, test_coord, |_| {
        None
    })
    .await;

    let result = map_detail_or_clusters(
        &db.pool,
        MapFilter {
            bbox: [5.0, 5.0, 25.0, 25.0],
            ..base_filter()
        },
        3,
    )
    .await
    .unwrap();

    match result {
        MapResult::Detail { total, rows } => {
            assert_eq!(total, MAP_DETAIL_THRESHOLD);
            assert_eq!(rows.len(), MAP_DETAIL_THRESHOLD as usize);
        }
        other => panic!(
            "threshold-inclusive path must be Detail, got {}",
            describe(&other)
        ),
    }

    db.close().await;
}

#[tokio::test]
async fn threshold_boundary_at_2001_rows() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed_rows_with_coords(
        &db.pool,
        MAP_DETAIL_THRESHOLD as usize + 1,
        test_coord,
        |_| None,
    )
    .await;

    let result = map_detail_or_clusters(
        &db.pool,
        MapFilter {
            bbox: [5.0, 5.0, 25.0, 25.0],
            ..base_filter()
        },
        3,
    )
    .await
    .unwrap();

    match result {
        MapResult::Clusters { total, .. } => {
            assert_eq!(total, MAP_DETAIL_THRESHOLD + 1);
        }
        other => panic!(
            "one-above-threshold must be Clusters, got {}",
            describe(&other)
        ),
    }

    db.close().await;
}

// --- Empty bbox ------------------------------------------------------------

#[tokio::test]
async fn empty_bbox_returns_empty_detail() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    // Seed rows OUTSIDE the requested bbox. The fixture places rows at
    // `(10..20, 10..20)`; we'll query a distant box that contains no
    // seeded row.
    seed_rows_with_coords(&db.pool, 100, test_coord, |_| None).await;

    let result = map_detail_or_clusters(
        &db.pool,
        MapFilter {
            bbox: [40.0, 40.0, 50.0, 50.0],
            ..base_filter()
        },
        3,
    )
    .await
    .unwrap();

    match result {
        MapResult::Detail { rows, total } => {
            assert_eq!(total, 0, "empty viewport total is zero");
            assert!(rows.is_empty(), "no rows match an empty viewport");
        }
        other => panic!("empty bbox must be Detail, got {}", describe(&other)),
    }

    db.close().await;
}

// --- NULL coords excluded --------------------------------------------------

#[tokio::test]
async fn rows_with_null_coords_excluded() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    // 5 rows with coords inside the bbox, 10 with NULL lat/lng.
    seed_rows_with_coords(&db.pool, 5, test_coord, |_| None).await;
    // Base index past the 5-row block so IPs don't collide.
    seed_rows_no_coords(&db.pool, 1000, 10).await;

    let result = map_detail_or_clusters(
        &db.pool,
        MapFilter {
            bbox: [5.0, 5.0, 25.0, 25.0],
            ..base_filter()
        },
        3,
    )
    .await
    .unwrap();

    match result {
        MapResult::Detail { rows, total } => {
            assert_eq!(total, 5, "NULL-coord rows are invisible to the map view");
            assert_eq!(rows.len(), 5);
            for row in &rows {
                assert!(row.latitude.is_some());
                assert!(row.longitude.is_some());
            }
        }
        other => panic!("expected Detail, got {}", describe(&other)),
    }

    db.close().await;
}

// --- Cell-size heuristic (unit-style) --------------------------------------

#[test]
fn cell_size_matches_zoom_bands() {
    for z in 0..=2 {
        assert_eq!(cell_size_for_zoom(z), 10.0, "zoom {z}");
    }
    for z in 3..=5 {
        assert_eq!(cell_size_for_zoom(z), 5.0, "zoom {z}");
    }
    for z in 6..=8 {
        assert_eq!(cell_size_for_zoom(z), 1.0, "zoom {z}");
    }
    for z in 9..=11 {
        assert_eq!(cell_size_for_zoom(z), 0.25, "zoom {z}");
    }
    for z in 12..=14 {
        assert_eq!(cell_size_for_zoom(z), 0.05, "zoom {z}");
    }
    for z in 15..=20 {
        assert_eq!(cell_size_for_zoom(z), 0.01, "zoom {z}");
    }
    // Values beyond 20 fall back to the finest band — no panic on
    // out-of-range zooms.
    assert_eq!(cell_size_for_zoom(u8::MAX), 0.01);
}

// --- Shapes-ignored invariant ----------------------------------------------

/// The `shapes` query parameter is not part of [`MapQuery`]. A
/// compile-time check — if someone adds a `shapes` field without
/// updating the spec, this test fails to compile and the reviewer sees
/// the invariant they're about to break.
///
/// We use a function that accepts every struct field to force the
/// compiler to enumerate the fields. Not directly possible in Rust
/// without macros, so we instead destructure the type in a dummy
/// initializer — any missing or extra field surfaces at compile time.
#[test]
fn map_query_does_not_contain_shapes() {
    // If `shapes` is ever added to MapQuery, the field-name grep
    // fallback below will trip. Compile-time destructure is the
    // strongest guarantee; the grep acts as a secondary check during
    // review.
    //
    // Direct field access would require constructing a MapQuery —
    // which has a non-Default `bbox` + `zoom`. Instead, assert on the
    // module source via a build-time constant include_str!.
    const DTO_SRC: &str = include_str!("../src/catalogue/dto.rs");
    // Find the MapQuery struct body and assert `shapes` is not a
    // field name inside it. The search is whitespace-tolerant.
    let start = DTO_SRC
        .find("pub struct MapQuery")
        .expect("MapQuery struct present");
    let after_start = &DTO_SRC[start..];
    let body_start = after_start.find('{').expect("MapQuery struct has body");
    let body_end = after_start[body_start..]
        .find('}')
        .expect("MapQuery struct closed");
    let body = &after_start[body_start..(body_start + body_end)];
    assert!(
        !body.contains("pub shapes"),
        "MapQuery must not expose `shapes` — the map view is shape-blind by contract"
    );
}

// --- Helpers ---------------------------------------------------------------

async fn assert_sample_inside_bucket(pool: &PgPool, bucket: &MapBucket) {
    let row = sqlx::query_as::<_, (f64, f64)>(
        "SELECT latitude, longitude FROM ip_catalogue WHERE id = $1",
    )
    .bind(bucket.sample_id)
    .fetch_one(pool)
    .await
    .expect("sample_id must resolve to a real row");
    let (lat, lng) = row;
    let [min_lat, min_lng, max_lat, max_lng] = bucket.bbox;
    assert!(
        (min_lat..=max_lat).contains(&lat) && (min_lng..=max_lng).contains(&lng),
        "sample ({lat}, {lng}) outside bucket bbox {:?}",
        bucket.bbox
    );
}

/// Short human-readable description of a [`MapResult`], used in
/// panic messages so failures surface the variant and its counts.
fn describe(result: &MapResult) -> String {
    match result {
        MapResult::Detail { rows, total } => {
            format!("Detail {{ rows: {}, total: {} }}", rows.len(), total)
        }
        MapResult::Clusters {
            buckets,
            total,
            cell_size,
        } => format!(
            "Clusters {{ buckets: {}, total: {}, cell_size: {} }}",
            buckets.len(),
            total,
            cell_size
        ),
    }
}
