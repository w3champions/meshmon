# `components/map`

Leaflet-based draw-and-display map primitive. Used by
`components/catalogue/CatalogueMap` to render catalogue pins and to
accept drawn filter shapes.

## Files

| File | Role |
|---|---|
| `DrawMap.tsx` | Controlled/uncontrolled map with geoman draw toolbar. Accepts `shapes` + `onShapesChange` for drawn regions, optional `pins` for markers, `onClusterClick` for the client-side cluster wrapper, and `onViewportChange` for server-side map queries. The `clusterMode` flag bypasses client clustering when the server has already aggregated. |

## `DrawMap` API

```tsx
<DrawMap
  shapes={GeoShape[]}
  onShapesChange={(next: GeoShape[]) => void}
  pins={DrawMapPin[]}                         // optional
  onClusterClick={(pinIds: string[]) => void} // optional — client clustering
  onViewportChange={(bbox, zoom) => void}     // optional — server paging
  clusterMode={boolean}                       // default false
  className={string}                          // optional
/>
```

- **`shapes`** — the current set of drawn shapes. `DrawMap` is
  primarily uncontrolled on the draw side: users draw shapes and the
  component emits them via `onShapesChange`. The `shapes` prop is only
  read to detect the external "clear" signal: when it transitions from
  non-empty to empty, the component removes all geoman draw layers from
  the map without emitting a change event (a `suppressEmit` ref
  prevents the loop).
- **`onShapesChange`** — fires after every `pm:create`, `pm:edit`, or
  `pm:remove` event with the full current shape list.
- **`pins`** — optional array of `{ id, lat, lon, popup?, icon?, onClick? }`
  rendered as Leaflet markers. Popup content is an arbitrary `ReactNode`
  injected via `react-leaflet`'s `<Popup>`. `icon` supports custom
  `L.DivIcon` / `L.Icon` (catalogue clusters use a sqrt-scaled bubble);
  `onClick` wires directly onto Leaflet's marker `click` event so
  server-aggregated bubbles can open the cluster dialog without going
  through `MarkerClusterGroup`.
- **`onClusterClick`** — only active while `clusterMode` is `false`.
  Invoked when the operator clicks a client-side cluster produced by
  `MarkerClusterGroup`; receives the ids of every pin in the cluster.
- **`onViewportChange`** — fires on Leaflet's `moveend` event (pan and
  zoom both trigger it) and once on mount with the initial viewport.
  Payload is `(bbox: [minLat, minLng, maxLat, maxLng], zoom: number)`
  matching the `Bbox` tuple and the server's `MapQuery.bbox` wire shape.
  The catalogue page feeds this straight into
  `useCatalogueMap(bbox, zoom, filters)`.
- **`clusterMode`** — when `true`, `DrawMap` bypasses
  `react-leaflet-cluster` and renders each pin as a plain marker. The
  catalogue map sets this when the server response is
  `kind: "clusters"` so pre-aggregated bubbles aren't re-clustered
  client-side.

The map is centred on `[20, 0]` at zoom 2 on mount; `worldCopyJump`
is enabled so panning across the antimeridian doesn't break marker
placement.

## Catalogue map contract — shapes excluded

`CatalogueMap` wraps `DrawMap` with the catalogue-specific wire
protocol. Two rules follow from the backend's `MapQuery` surface:

1. **Shapes are excluded from the map query.** Operators draw shapes
   against the unfiltered fleet geography so they aren't drawing blind.
   `shapes` only narrow the table query.
2. **The map response is `detail` vs `clusters`, picked server-side.**
   Below the detail threshold the backend returns raw rows; above it,
   grid-aggregated buckets keyed by zoom-level cell size.
   `CatalogueMap` branches on `response.kind` and passes `clusterMode`
   into `DrawMap` so client-side clustering is suppressed during
   `clusters` responses.

## `GeoShape` semantics (`lib/geo.ts`)

All coordinates use GeoJSON order `[longitude, latitude]`.

| Kind | Valid when |
|---|---|
| `polygon` | `coordinates` has ≥ 3 vertices. |
| `rectangle` | `sw` is strictly south-west of `ne` (both lng and lat). |
| `circle` | `radiusMeters > 0`. |

`pointInShapes(lat, lon, shapes)` returns `true` if the point is
inside any shape (OR). Shapes are tested in order; the first match
short-circuits. An empty `shapes` array always returns `false`.

`boundingBoxOf(shapes)` returns `[minLng, minLat, maxLng, maxLat]`
enclosing all shapes, or `null` for an empty array.

`shapesToPolygons(shapes)` serialises the shape array into the
backend's `Polygon[]` wire shape (arrays of `[lng, lat]` rings). Used
by the catalogue table query to round-trip shapes through the server's
point-in-polygon filter.

Internally, all shapes are approximated as turf polygons
(`@turf/boolean-point-in-polygon`). Circles are discretised to a
64-step polygon approximation matching what geoman renders visually.

## Draw toolbar

Enabled modes: rectangle, polygon, circle, drag, edit, remove.
Disabled: marker, circle-marker, polyline, text, cut, rotate.
Controls are positioned at `topright`.

## Limitations

Shapes that cross the antimeridian (±180° longitude) are not
supported. The bounding-box computation and turf containment tests
assume a continuous longitude range. This is a turf limitation shared
by the map filter.

## Operator-locked fields

`DrawMap` is a display and draw primitive only — it has no knowledge
of the catalogue lock model. `CatalogueMap` converts `CatalogueEntry`
rows into pins; the lock semantics live in `EntryDrawer`.
