/**
 * Shape-based spatial filtering helpers.
 *
 * All coordinates use GeoJSON order `[longitude, latitude]` to stay
 * composable with the turf primitives we depend on, and all bboxes are
 * returned in GeoJSON bbox order `[minLng, minLat, maxLng, maxLat]`.
 * Consumers that interoperate with Leaflet must convert explicitly —
 * Leaflet's `LatLngBounds` uses `[south, west]` / `[north, east]` pairs.
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
  const ring = closeRing(coordinates.map(([lng, lat]) => [lng, lat]));
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
