# FilterRail

Reusable filter rail for the catalogue surface. Each group is a native
`<details>` collapsible; facet groups render a searchable, count-annotated
multi-select; free-text groups render a single-line search input; the map
group is a summary row with an `Open map` / `Edit map` action.

## `FilterValue`

```ts
interface FilterValue {
  countryCodes: string[];   // CountryFacet.code
  asns: number[];           // AsnFacet.asn (always a real number)
  networks: string[];       // NetworkFacet.name
  cities: string[];         // CityFacet.name
  ipPrefix?: string;        // free text; undefined when empty
  nameSearch?: string;      // free text; undefined when empty
  shapes: GeoShape[];       // from @/lib/geo
}
```

All facet ids come from the openapi-generated `FacetsResponse` under
`@/api/schema.gen`. `GeoShape` comes from `@/lib/geo`.

## Facet sources

| Group    | Source facet   | Id key | Display         |
|----------|----------------|--------|-----------------|
| Country  | `countries[]`  | `code` | `name (code)` or `code` |
| ASN      | `asns[]`       | `asn`  | `AS{asn}`       |
| Network  | `networks[]`   | `name` | `name`          |
| City     | `cities[]`     | `name` | `name`          |

Each facet group renders the top 50 options by count, descending. A
per-group search box filters first; the 50-option cap is applied after
the filter so typing can reach items that would otherwise be clipped.
When the filtered list is longer than 50 the UI surfaces a hint telling
the operator to narrow the search.

## Empty facets

When `facets` is `undefined` (loading or failure) each group renders a
neutral "Facets unavailable" hint instead of an empty list. No crash,
no fake options. Free-text groups remain usable because they do not
depend on facet data.

## Shape emission

The shape group emits `GeoShape[]` exactly as produced by the map
surface; `FilterRail` does not create or mutate shapes itself. Clearing
shapes dispatches `onChange({ ...value, shapes: [] })`.

## `onOpenMap`

Optional. When provided, the shape group's primary button (labelled
`Open map` when no shapes are present, `Edit map` otherwise) invokes
the callback. When omitted the button is disabled — the parent decides
how to mount the draw surface.

## Null-ASN note

Rows whose ASN is `NULL` in the catalogue are currently not surfaced
as a facet bucket; filtering by "Unknown ASN" is not supported in
this release.

## Reuse

The component exposes a minimal `value` / `onChange` / `facets`
contract so other filter surfaces can reuse it. Persist `FilterValue`
wherever makes sense for the host surface (search params, a Zustand
store, or a parent `useState`) and hand the same callback back in.
