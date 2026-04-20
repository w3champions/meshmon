import { describe, expect, test } from "vitest";
import { boundingBoxOf, type GeoShape, pointInShapes } from "@/lib/geo";

describe("pointInShapes", () => {
  test("rectangle: point inside returns true", () => {
    const shapes: GeoShape[] = [{ kind: "rectangle", sw: [10, 20], ne: [30, 40] }];
    // lat=30, lon=20 → inside [lon 10..30, lat 20..40]
    expect(pointInShapes(30, 20, shapes)).toBe(true);
  });

  test("rectangle: point outside returns false", () => {
    const shapes: GeoShape[] = [{ kind: "rectangle", sw: [10, 20], ne: [30, 40] }];
    // lat=50, lon=20 → north of the rect
    expect(pointInShapes(50, 20, shapes)).toBe(false);
  });

  test("circle: point inside (center) returns true", () => {
    const shapes: GeoShape[] = [{ kind: "circle", center: [0, 0], radiusMeters: 10_000 }];
    expect(pointInShapes(0, 0, shapes)).toBe(true);
  });

  test("circle: point outside returns false", () => {
    const shapes: GeoShape[] = [{ kind: "circle", center: [0, 0], radiusMeters: 10_000 }];
    // ~111km east of origin — far outside a 10km circle
    expect(pointInShapes(0, 1, shapes)).toBe(false);
  });

  test("polygon: concave (hairpin) — point inside a lobe returns true", () => {
    // C-shaped polygon (notch cut from the right side)
    const shapes: GeoShape[] = [
      {
        kind: "polygon",
        coordinates: [
          [0, 0],
          [10, 0],
          [10, 4],
          [4, 4],
          [4, 6],
          [10, 6],
          [10, 10],
          [0, 10],
          [0, 0],
        ],
      },
    ];
    // Point inside the top lobe: lon=2, lat=8
    expect(pointInShapes(8, 2, shapes)).toBe(true);
  });

  test("polygon: concave (hairpin) — point inside the bite is outside", () => {
    // Same C-shape as above. The notch is the rectangle lon 4..10, lat 4..6.
    const shapes: GeoShape[] = [
      {
        kind: "polygon",
        coordinates: [
          [0, 0],
          [10, 0],
          [10, 4],
          [4, 4],
          [4, 6],
          [10, 6],
          [10, 10],
          [0, 10],
          [0, 0],
        ],
      },
    ];
    // Point inside the bite: lon=7, lat=5 → outside the polygon
    expect(pointInShapes(5, 7, shapes)).toBe(false);
  });

  test("polygon: auto-closes an unclosed ring", () => {
    // Unit square missing the final closing vertex
    const shapes: GeoShape[] = [
      {
        kind: "polygon",
        coordinates: [
          [0, 0],
          [1, 0],
          [1, 1],
          [0, 1],
        ],
      },
    ];
    expect(pointInShapes(0.5, 0.5, shapes)).toBe(true);
    expect(pointInShapes(2, 2, shapes)).toBe(false);
  });

  test("multiple shapes compose with OR", () => {
    const shapes: GeoShape[] = [
      { kind: "rectangle", sw: [0, 0], ne: [1, 1] },
      { kind: "rectangle", sw: [10, 10], ne: [11, 11] },
    ];
    // lat=10.5, lon=10.5 → inside the second rect only
    expect(pointInShapes(10.5, 10.5, shapes)).toBe(true);
  });

  test("empty shapes array always returns false", () => {
    expect(pointInShapes(0, 0, [])).toBe(false);
    expect(pointInShapes(47.3, 8.5, [])).toBe(false);
  });

  test("polygon with fewer than 3 vertices throws", () => {
    expect(() =>
      pointInShapes(0, 0, [
        {
          kind: "polygon",
          coordinates: [
            [0, 0],
            [1, 1],
          ],
        },
      ]),
    ).toThrow(/3 vertices/);
  });

  test("rectangle with inverted sw/ne throws", () => {
    expect(() => pointInShapes(0, 0, [{ kind: "rectangle", sw: [30, 40], ne: [10, 20] }])).toThrow(
      /south-west/,
    );
  });

  test("circle with non-positive radius throws", () => {
    expect(() =>
      pointInShapes(0, 0, [{ kind: "circle", center: [0, 0], radiusMeters: 0 }]),
    ).toThrow(/positive/);
  });
});

describe("boundingBoxOf", () => {
  test("empty shapes returns null", () => {
    expect(boundingBoxOf([])).toBeNull();
  });

  test("single rectangle returns its exact bounds", () => {
    const shapes: GeoShape[] = [{ kind: "rectangle", sw: [10, 20], ne: [30, 40] }];
    // GeoJSON bbox order: [minLng, minLat, maxLng, maxLat]
    expect(boundingBoxOf(shapes)).toEqual([10, 20, 30, 40]);
  });

  test("single polygon returns bounds covering every vertex", () => {
    const shapes: GeoShape[] = [
      {
        kind: "polygon",
        coordinates: [
          [-5, -2],
          [7, -2],
          [7, 3],
          [-5, 3],
          [-5, -2],
        ],
      },
    ];
    expect(boundingBoxOf(shapes)).toEqual([-5, -2, 7, 3]);
  });

  test("boundingBoxOf a circle returns symmetric bounds around the center", () => {
    // 100km radius circle at (0, 0). Expected half-extent in degrees is
    // (radius in metres) / (earth radius) / (π / 180) ≈ 100000 / 111000 ≈ 0.9°.
    // Latitude extent is exactly symmetric in principle; longitude extent at
    // the equator (cos(lat) = 1) matches. The turf polygon discretization
    // introduces a small approximation error, so use a generous tolerance.
    const expectedHalfExtent = 100_000 / 111_000; // ~0.9009 degrees
    const tolerance = 0.1;
    const shapes: GeoShape[] = [{ kind: "circle", center: [0, 0], radiusMeters: 100_000 }];
    const bbox = boundingBoxOf(shapes);
    expect(bbox).not.toBeNull();
    if (bbox === null) return;
    const [minLng, minLat, maxLng, maxLat] = bbox;
    expect(minLng).toBeCloseTo(-expectedHalfExtent, 0);
    expect(maxLng).toBeCloseTo(expectedHalfExtent, 0);
    expect(minLat).toBeCloseTo(-expectedHalfExtent, 0);
    expect(maxLat).toBeCloseTo(expectedHalfExtent, 0);
    // Symmetry assertions within tolerance
    expect(Math.abs(minLng + maxLng)).toBeLessThan(tolerance);
    expect(Math.abs(minLat + maxLat)).toBeLessThan(tolerance);
  });

  test("union of rect + circle returns the enclosing bbox", () => {
    const shapes: GeoShape[] = [
      { kind: "rectangle", sw: [0, 0], ne: [10, 10] },
      { kind: "circle", center: [20, 20], radiusMeters: 1_000 },
    ];
    const bbox = boundingBoxOf(shapes);
    expect(bbox).not.toBeNull();
    if (bbox === null) return;
    const [minLng, minLat, maxLng, maxLat] = bbox;
    // Rectangle dominates the south-west corner
    expect(minLng).toBeCloseTo(0, 5);
    expect(minLat).toBeCloseTo(0, 5);
    // Circle extends ~0.009° east and north of (20, 20) for a 1km radius
    expect(maxLng).toBeGreaterThan(20);
    expect(maxLat).toBeGreaterThan(20);
    // Sanity: within 0.05° of the circle's centre
    expect(maxLng).toBeLessThan(20.05);
    expect(maxLat).toBeLessThan(20.05);
  });
});
