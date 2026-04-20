/**
 * Shape-based spatial filtering helpers.
 *
 * All coordinates use GeoJSON order `[longitude, latitude]` to stay
 * composable with the turf primitives we depend on, and all bboxes are
 * returned in GeoJSON bbox order `[minLng, minLat, maxLng, maxLat]`.
 * Consumers that interoperate with Leaflet must convert explicitly —
 * Leaflet's `LatLngBounds` uses `[south, west]` / `[north, east]` pairs.
 *
 * @remarks
 * Antimeridian-wrapping shapes (a rectangle straddling ±180°, a polygon
 * crossing the dateline) are not supported. The helpers assume all rings
 * are single-segment on a continuous lng range. This is a turf limitation,
 * not a meshmon one.
 */

import booleanPointInPolygon from "@turf/boolean-point-in-polygon";
import circle from "@turf/circle";
import type { Feature, Polygon, Position } from "geojson";

/**
 * Supported shape kinds. Coordinates are always `[longitude, latitude]`.
 *
 * - `polygon.coordinates`: an outer ring; the first/last vertex may be
 *   omitted and will be closed automatically.
 * - `rectangle.sw` / `rectangle.ne`: south-west and north-east corners
 *   of an axis-aligned box.
 * - `circle.center`: centre point; `radiusMeters` is the geodesic radius.
 */
export type GeoShape =
  | { kind: "polygon"; coordinates: [number, number][] }
  | { kind: "rectangle"; sw: [number, number]; ne: [number, number] }
  | { kind: "circle"; center: [number, number]; radiusMeters: number };

/**
 * Axis-aligned bounding box used by the catalogue map surface.
 *
 * Tuple ordering: `[minLat, minLng, maxLat, maxLng]` — matches the
 * `MapBucket.bbox` wire type emitted by the backend (`lat` first, `lng`
 * second). Consumers that interoperate with Leaflet `LatLngBounds` can
 * read the tuple directly; turf helpers in this module keep their own
 * `[lng, lat]` ordering, so do not mix them with `Bbox` without an
 * explicit conversion.
 */
export type Bbox = [number, number, number, number];

const CIRCLE_STEPS = 64;

function closeRing(ring: Position[]): Position[] {
  if (ring.length === 0) return ring;
  const [firstLng, firstLat] = ring[0];
  const last = ring[ring.length - 1];
  if (last[0] === firstLng && last[1] === firstLat) return ring;
  return [...ring, [firstLng, firstLat]];
}

function rectangleToPolygon(sw: [number, number], ne: [number, number]): Feature<Polygon> {
  const [swLng, swLat] = sw;
  const [neLng, neLat] = ne;
  if (swLng >= neLng || swLat >= neLat) {
    throw new Error(
      `rectangle sw must be strictly south-west of ne; got sw=[${swLng},${swLat}] ne=[${neLng},${neLat}]`,
    );
  }
  const coordinates: Position[] = [
    [swLng, swLat],
    [neLng, swLat],
    [neLng, neLat],
    [swLng, neLat],
    [swLng, swLat],
  ];
  return {
    type: "Feature",
    properties: {},
    geometry: { type: "Polygon", coordinates: [coordinates] },
  };
}

function polygonToFeature(coordinates: [number, number][]): Feature<Polygon> {
  if (coordinates.length < 3) {
    throw new Error(`polygon requires at least 3 vertices; got ${coordinates.length}`);
  }
  const ring = closeRing(coordinates);
  return {
    type: "Feature",
    properties: {},
    geometry: { type: "Polygon", coordinates: [ring] },
  };
}

function circleToPolygon(center: [number, number], radiusMeters: number): Feature<Polygon> {
  // `@turf/circle` returns a regular-polygon approximation; 64 steps keeps
  // the area error under ~0.1% and matches what geoman draws visually.
  return circle(center, radiusMeters / 1000, {
    steps: CIRCLE_STEPS,
    units: "kilometers",
  });
}

function shapeToPolygon(shape: GeoShape): Feature<Polygon> {
  switch (shape.kind) {
    case "rectangle":
      return rectangleToPolygon(shape.sw, shape.ne);
    case "polygon":
      return polygonToFeature(shape.coordinates);
    case "circle":
      if (shape.radiusMeters <= 0) {
        throw new Error(`circle radius must be positive; got ${shape.radiusMeters}`);
      }
      return circleToPolygon(shape.center, shape.radiusMeters);
  }
}

/**
 * Returns `true` if `(lat, lon)` falls inside any of the provided shapes.
 *
 * Shapes compose with a logical OR — the first shape that contains the
 * point short-circuits. An empty `shapes` array always returns `false`.
 */
export function pointInShapes(lat: number, lon: number, shapes: GeoShape[]): boolean {
  if (shapes.length === 0) return false;
  const point: Feature<{ type: "Point"; coordinates: Position }> = {
    type: "Feature",
    properties: {},
    geometry: { type: "Point", coordinates: [lon, lat] },
  };
  for (const shape of shapes) {
    const polygon = shapeToPolygon(shape);
    if (booleanPointInPolygon(point, polygon)) return true;
  }
  return false;
}

/**
 * Serialises `GeoShape[]` into the backend's `Polygon[]` wire form for
 * the `shapes` query parameter.
 *
 * Rectangles expand to their four corners (closed ring), circles are
 * polygonalised via `@turf/circle` with the same 64-step discretisation
 * {@link pointInShapes} uses, and free-form polygons pass through with
 * their ring closed. Every ring is emitted in GeoJSON order
 * `[lng, lat]` to match `crates/service/src/catalogue/shapes.rs`'s
 * `Polygon(Vec<[f64; 2]>)` wire shape.
 */
export function shapesToPolygons(shapes: GeoShape[]): [number, number][][] {
  return shapes.map((shape) => {
    const ring = shapeToPolygon(shape).geometry.coordinates[0];
    return ring.map((position) => [position[0], position[1]] as [number, number]);
  });
}

/**
 * Returns the GeoJSON bbox `[minLng, minLat, maxLng, maxLat]` that
 * encloses every vertex of every shape, or `null` when `shapes` is empty.
 *
 * Circles are approximated with the same polygon discretization used
 * by {@link pointInShapes}, so the bbox matches the containment test.
 */
export function boundingBoxOf(shapes: GeoShape[]): [number, number, number, number] | null {
  if (shapes.length === 0) return null;
  let minLng = Number.POSITIVE_INFINITY;
  let minLat = Number.POSITIVE_INFINITY;
  let maxLng = Number.NEGATIVE_INFINITY;
  let maxLat = Number.NEGATIVE_INFINITY;
  for (const shape of shapes) {
    const ring = shapeToPolygon(shape).geometry.coordinates[0];
    for (const [lng, lat] of ring) {
      if (lng < minLng) minLng = lng;
      if (lat < minLat) minLat = lat;
      if (lng > maxLng) maxLng = lng;
      if (lat > maxLat) maxLat = lat;
    }
  }
  if (!Number.isFinite(minLng)) return null;
  return [minLng, minLat, maxLng, maxLat];
}
