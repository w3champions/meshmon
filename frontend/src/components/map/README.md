# `components/map`

Leaflet-based draw-and-display map primitive. Used by
`components/catalogue/CatalogueMap` to render catalogue pins and to
accept drawn filter shapes.

## Files

| File | Role |
|---|---|
| `DrawMap.tsx` | Controlled/uncontrolled map with geoman draw toolbar. Accepts `shapes` + `onShapesChange` for drawn regions and optional `pins` for markers. |

## `DrawMap` API

```tsx
<DrawMap
  shapes={GeoShape[]}
  onShapesChange={(next: GeoShape[]) => void}
  pins={DrawMapPin[]}   // optional
  className={string}    // optional — applied to the wrapper div
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
- **`pins`** — optional array of `{ id, lat, lon, popup? }` rendered
  as Leaflet markers. Popup content is an arbitrary `ReactNode` injected
  via `react-leaflet`'s `<Popup>`.

The map is centred on `[20, 0]` at zoom 2 on mount; `worldCopyJump`
is enabled so panning across the antimeridian doesn't break marker
placement.

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
