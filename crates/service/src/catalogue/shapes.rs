//! Polygon geometry for the `shapes` catalogue filter.
//!
//! This module owns both the wire type ([`Polygon`]) and the runtime
//! machinery that converts it into a [`geo::Polygon`] and answers
//! point-in-polygon queries. The list handler uses these in a
//! two-stage filter:
//!
//! 1. [`union_bbox`] yields an axis-aligned bbox over every polygon's
//!    vertices. That bbox goes into the SQL `WHERE` clause as a cheap
//!    pre-filter — rows outside the union bbox are impossible matches
//!    regardless of polygon shape.
//! 2. [`point_in_any`] then runs exact point-in-polygon over the rows
//!    returned from SQL. The pre-converted `geo::Polygon` slice is
//!    built once per request via [`TryFrom<&Polygon> for
//!    geo::Polygon<f64>`], so the caller eats the conversion cost one
//!    time rather than once per row.
//!
//! The wire shape is GeoJSON-compatible (`[lng, lat]` pairs) so the
//! frontend can feed `@turf/helpers` polygons straight through. All
//! conversions into `geo` swap to `(x=lng, y=lat)` on the way in — the
//! `geo` crate is x/y, not lat/lng.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// GeoJSON-compatible polygon ring expressed as `[lng, lat]` pairs.
///
/// The ring is implicitly closed — the `TryFrom` impl below does not
/// require an explicit closing vertex. A minimum of three *distinct*
/// points is required; shorter rings (two vertices or all-identical
/// points) are rejected as [`ShapeError::TooFewPoints`].
///
/// The `[lng, lat]` order matches GeoJSON and `@turf/helpers`, so the
/// frontend can feed Turf outputs straight through without reordering.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct Polygon(pub Vec<[f64; 2]>);

/// Failure modes surfaced when converting a wire [`Polygon`] into a
/// [`geo::Polygon`].
#[derive(Debug, thiserror::Error)]
pub enum ShapeError {
    /// The ring has fewer than three distinct vertices after deduping
    /// consecutive duplicates. A "polygon" with two or fewer distinct
    /// points has no interior, so it can never match any row.
    #[error("polygon needs at least 3 distinct points (got {got})")]
    TooFewPoints {
        /// Number of distinct points observed.
        got: usize,
    },
}

/// Convert a wire polygon (GeoJSON-style `[lng, lat]` ring) into a
/// [`geo::Polygon`] suitable for `Contains` queries.
///
/// Rejects rings with fewer than three distinct vertices. Consecutive
/// identical points are deduped before counting — GeoJSON rings often
/// repeat the first vertex at the end to close the ring, and we must
/// not count that duplicate when judging "enough distinct points".
///
/// `geo::Polygon::new` treats the line string as implicitly closed, so
/// the converter does *not* append the closing vertex itself.
impl TryFrom<&Polygon> for geo::Polygon<f64> {
    type Error = ShapeError;

    fn try_from(p: &Polygon) -> Result<Self, Self::Error> {
        // Dedup consecutive identical points before counting distinct
        // vertices. We iterate once, emitting `geo::Coord` values and
        // skipping any point equal to its immediate predecessor.
        let mut coords: Vec<geo::Coord<f64>> = Vec::with_capacity(p.0.len());
        for [lng, lat] in p.0.iter().copied() {
            let next = geo::Coord { x: lng, y: lat };
            if coords.last().copied() != Some(next) {
                coords.push(next);
            }
        }
        // A closed ring ends with a repeat of its first vertex in the
        // wire form. `geo::Polygon::new` treats the LineString as
        // implicitly closed, so for the "distinct points" count we
        // additionally drop a trailing vertex that equals the first.
        if coords.len() >= 2 && coords.first() == coords.last() {
            coords.pop();
        }
        if coords.len() < 3 {
            return Err(ShapeError::TooFewPoints { got: coords.len() });
        }
        let line_string = geo::LineString::new(coords);
        Ok(geo::Polygon::new(line_string, Vec::new()))
    }
}

/// Axis-aligned bounding box across every vertex of every polygon.
///
/// Returns `[min_lat, min_lon, max_lat, max_lon]`, matching the shape
/// used by the existing `bbox` query-string filter on [`super::dto::
/// ListQuery`]. Returns `None` when `polys` is empty or when every
/// polygon is empty (no vertices to fold over).
///
/// Note the axis swap: the wire shape stores `[lng, lat]` (point `[0]`
/// is longitude), but the returned bbox leads with latitude so the SQL
/// pre-filter can drop it straight into the existing bbox column order
/// without renaming.
pub fn union_bbox(polys: &[Polygon]) -> Option<[f64; 4]> {
    let mut min_lat = f64::INFINITY;
    let mut min_lon = f64::INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    let mut seen_any = false;

    for poly in polys {
        for [lng, lat] in poly.0.iter().copied() {
            seen_any = true;
            if lat < min_lat {
                min_lat = lat;
            }
            if lat > max_lat {
                max_lat = lat;
            }
            if lng < min_lon {
                min_lon = lng;
            }
            if lng > max_lon {
                max_lon = lng;
            }
        }
    }

    if seen_any {
        Some([min_lat, min_lon, max_lat, max_lon])
    } else {
        None
    }
}

/// Point-in-any-polygon test. Returns `true` iff `(lat, lng)` falls
/// inside at least one polygon in `polys`. Short-circuits on the first
/// match.
///
/// The slice holds `geo::Polygon<f64>` rather than the wire [`Polygon`]
/// on purpose: the caller pays the [`TryFrom`] conversion cost once
/// per request, not once per row. The list handler builds this slice
/// in its request-setup step and then hands it to the row loop.
pub fn point_in_any(polys: &[geo::Polygon<f64>], lat: f64, lng: f64) -> bool {
    use geo::Contains;
    // `geo` is `(x, y)`, which for lat/lng data means `(lng, lat)`.
    let p = geo::Point::new(lng, lat);
    polys.iter().any(|poly| poly.contains(&p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit square in `[lng, lat]` coordinates centred at the origin,
    /// with corners at `±1.0`.
    fn unit_square_at_origin() -> Polygon {
        Polygon(vec![[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]])
    }

    /// A second disjoint square well away from the origin — shifted to
    /// `[9, 11]` on both axes so there is no overlap with the origin
    /// square.
    fn disjoint_square_far_east() -> Polygon {
        Polygon(vec![[9.0, 9.0], [11.0, 9.0], [11.0, 11.0], [9.0, 11.0]])
    }

    #[test]
    fn try_from_accepts_three_distinct_points() {
        let triangle = Polygon(vec![[0.0, 0.0], [1.0, 0.0], [0.5, 1.0]]);
        let geo_poly: geo::Polygon<f64> = (&triangle)
            .try_into()
            .expect("3 distinct points must convert");
        // The converted polygon must answer a straightforward contains
        // query — point at centroid is inside, point far away is not.
        let centroid = geo::Point::new(0.5, 0.3);
        let far = geo::Point::new(5.0, 5.0);
        use geo::Contains;
        assert!(geo_poly.contains(&centroid), "centroid must be inside");
        assert!(!geo_poly.contains(&far), "far point must be outside");
    }

    #[test]
    fn try_from_rejects_two_point_ring() {
        let degenerate = Polygon(vec![[0.0, 0.0], [1.0, 1.0]]);
        let err = <&Polygon as TryInto<geo::Polygon<f64>>>::try_into(&degenerate)
            .expect_err("2 points must not yield a polygon");
        assert!(
            matches!(err, ShapeError::TooFewPoints { got: 2 }),
            "got: {err:?}"
        );
    }

    #[test]
    fn try_from_rejects_all_identical_points() {
        // Three identical points dedup down to a single distinct coord.
        let identical = Polygon(vec![[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]);
        let err = <&Polygon as TryInto<geo::Polygon<f64>>>::try_into(&identical)
            .expect_err("all-identical points must not yield a polygon");
        assert!(
            matches!(err, ShapeError::TooFewPoints { got: 1 }),
            "got: {err:?}"
        );
    }

    #[test]
    fn point_in_any_detects_inside_point_of_single_polygon() {
        let square: geo::Polygon<f64> = (&unit_square_at_origin())
            .try_into()
            .expect("unit square converts");
        let polys = vec![square];
        let inside_point = (0.0, 0.0);
        let outside_point = (2.0, 2.0);

        assert!(point_in_any(&polys, inside_point.0, inside_point.1));
        assert!(!point_in_any(&polys, outside_point.0, outside_point.1));
    }

    #[test]
    fn point_in_any_or_semantics_across_multiple_polygons() {
        let origin_square: geo::Polygon<f64> = (&unit_square_at_origin())
            .try_into()
            .expect("origin square converts");
        let far_square: geo::Polygon<f64> = (&disjoint_square_far_east())
            .try_into()
            .expect("far square converts");
        let polys = vec![origin_square, far_square];

        // Point sits inside the far square but well outside the origin
        // square — ANY semantics must flag it as a match.
        let inside_far_only = (10.0, 10.0);
        assert!(point_in_any(&polys, inside_far_only.0, inside_far_only.1));

        // Point between the two disjoint squares — must NOT match.
        let between = (5.0, 5.0);
        assert!(!point_in_any(&polys, between.0, between.1));
    }

    #[test]
    fn union_bbox_single_square_returns_square_extent() {
        let polys = [unit_square_at_origin()];
        let bbox = union_bbox(&polys).expect("non-empty polygons yield a bbox");
        // `[min_lat, min_lon, max_lat, max_lon]`.
        assert_eq!(bbox, [-1.0, -1.0, 1.0, 1.0]);
    }

    #[test]
    fn union_bbox_two_disjoint_squares_spans_both() {
        let polys = [unit_square_at_origin(), disjoint_square_far_east()];
        let bbox = union_bbox(&polys).expect("two squares yield a bbox");
        // Origin square spans lat/lon -1..=1; far square spans 9..=11
        // on both axes. Union must be -1..=11 on both.
        assert_eq!(bbox, [-1.0, -1.0, 11.0, 11.0]);
    }

    #[test]
    fn union_bbox_empty_slice_returns_none() {
        let polys: [Polygon; 0] = [];
        assert!(union_bbox(&polys).is_none());
    }
}
