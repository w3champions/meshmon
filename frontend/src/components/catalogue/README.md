# `components/catalogue`

UI components for the `/catalogue` page. They are composed inside the
catalogue route and share a single SSE connection opened at the page level.

## Files

| File | Role |
|---|---|
| `CatalogueTable.tsx` | `@tanstack/react-table` table with sortable headers, row virtualization (`@tanstack/react-virtual`), column-visibility toggle (persisted to `localStorage`), and a Load-more control that pulls the next cursor. Rows are keyboard-accessible; clicking a row fires `onRowClick`. The per-row re-enrich button calls `onReenrich` without opening the drawer. |
| `CatalogueMap.tsx` | Branches on the server map response `kind`: `detail` renders one pin per row through the standard cluster wrapper; `clusters` renders one pre-aggregated bubble per bucket with the cluster wrapper bypassed. Pin popups include an "Open details" link that fires `onRowClick`; cluster bubbles fire `onClusterOpen(bbox)`. The `onViewportChange` callback feeds the parent's `useCatalogueMap(bbox, zoom, filters)` hook. |
| `CatalogueClusterDialog.tsx` | Modal opened when the operator clicks a server cluster bubble. Owns its own `useCatalogueListInfinite` scoped to the cluster's bbox + the active non-shape filters. Rows stream in pages of 50 via Load-more. |
| `EntryDrawer.tsx` | Right-side `Sheet` for editing a single entry. Uses `react-hook-form` + Zod; sends diff-only PATCH requests (only dirty fields are sent). Latitude + Longitude render as a single composite Location row via `components/map/LocationPicker`. Provides per-field "Revert to auto" links for operator-locked fields. Surfaces re-enrich and delete actions. |
| `PasteStaging.tsx` | Paste panel for bulk IP ingestion. Runs the client-side parser (`lib/catalogue-parse`) before POST so rejections surface immediately. An optional "Default metadata" panel applies display name, city, country, location, website, and notes to every accepted IP. After a successful POST, seeds the TanStack Query cache with server-returned entries, renders `StagingChip` per row (reads enrichment status live from the cache as SSE events arrive), and surfaces any `skipped_summary` the server returned. |
| `CountryPicker.tsx` | Thin wrapper around the `COUNTRIES` table that emits `{code, name}` atomically. Used by the paste-metadata panel so `country_code` and `country_name` travel as a pair — half-filled pairs would fail the backend's paired-atomicity rule. |
| `StatusChip.tsx` | Compact badge for the `enrichment_status` field (`pending`, `enriched`, `failed`). Appends an "Operator-edited" lock badge when `operatorLocked` is true. Optionally actionable (`onReenrich` prop) for `enriched` and `failed` states. |
| `ReenrichConfirm.tsx` | Modal confirmation dialog for bulk re-enrich. Parent owns the threshold logic (25-row gate); the dialog receives `selectionSize` and displays "~N ipgeolocation credits". |

## Data flow — server-driven paging, sort, and map aggregation

The page holds the shared state and passes slices down:

```
CataloguePage
├── FilterRail                (filter state + shapes → query params)
├── CatalogueTable            (visible when tab = "table")
├── CatalogueMap              (visible when tab = "map")
├── CatalogueClusterDialog    (visible when a cluster is selected)
├── EntryDrawer               (open when selectedId !== undefined)
├── PasteStaging              (shown in a sheet/dialog driven by "Add IPs")
└── ReenrichConfirm           (open when the bulk confirm is active)
```

### Table — infinite query

`useCatalogueListInfinite(tableQuery)` produces pages of up to 100
entries. The page flattens `data.pages.flatMap((p) => p.entries)` into
`rows` and passes them to `CatalogueTable` along with `total`,
`hasNextPage`, `isFetchingNextPage`, and `fetchNextPage`. The table
itself is stateless with respect to paging — Load-more calls
`fetchNextPage`, the server returns the next cursor, and react-query
concatenates pages for us.

`total` is always the server-reported count for the active filter
(pre-page). The "Re-enrich all" counter reads this total, not
`rows.length`, so the operator sees the true filter size even before
every page has been pulled. The bulk action itself fires against the
currently-loaded `rows`, since walking the full pagination just to hand
the server a list of ids it would have found anyway adds no value.

### Sort — URL round-trip

Sort state lives in the URL as `?sort=<col>&dir=<asc|desc>`. The page
derives `{ col, dir }` from `useSearch` and passes it into the table;
`onSortChange` writes the URL (`replace: true`) which re-renders the
page, feeds the new sort into `tableQuery`, and retriggers the infinite
query from a fresh cursor chain. Both `col` and `dir` are nullable: when
absent, the server falls back to `created_at DESC` tiebroken on
`id DESC`.

### Map — viewport-driven query

`CatalogueMap` publishes viewport changes through `onViewportChange`
(bbox + zoom). The page caches these in local state and drives
`useCatalogueMap(bbox, zoom, mapQuery)`. The backend returns one of two
shapes:

- **`kind: "detail"`** — raw rows, when the filtered viewport count is
  at or below the detail threshold. `CatalogueMap` renders a pin per
  row.
- **`kind: "clusters"`** — grid-aggregated buckets, when above the
  threshold. `CatalogueMap` renders a pre-aggregated bubble per bucket
  with the client-side cluster layer bypassed so the server's
  aggregation isn't re-clustered.

The map query intentionally omits `city`, `shapes`, and `sort` — they
aren't part of the `MapQuery` wire type. Operators draw shapes against
the unfiltered fleet geography so they aren't drawing blind; `city`
narrows the table but not the map.

### Cluster dialog — cell-scoped fetch

When the operator clicks a cluster bubble, the map fires
`onClusterOpen(bbox)`. The page stores the bbox and opens
`CatalogueClusterDialog`, which owns its own `useCatalogueListInfinite`
scoped to `bbox = cluster cell`, `pageSize = 50`, and the active
non-shape filters. Rows stream in pages of 50 via Load-more. Clicking
an entry closes the dialog and opens `EntryDrawer` for that id.

### SSE invalidation

`CatalogueStreamProvider` (in `src/api/`) mounts `useCatalogueStream`
once for the whole authenticated subtree from inside `AppShell`, so
every page that consumes catalogue-derived data — the catalogue page,
the campaign composer, campaign detail, the history pair page — sees
`CATALOGUE_LIST_KEY`, `CATALOGUE_MAP_KEY`, `CATALOGUE_FACETS_KEY`, and
per-entry caches invalidated on every catalogue event. Pages must not
mount the hook themselves.

## Operator-locked field semantics

`operator_edited_fields` on each `CatalogueEntry` is an array of
PascalCase field names. The full set the server tracks:

```
DisplayName  Asn  CountryCode  CountryName  City
Latitude  Longitude  NetworkOperator  Website  Notes
```

`EntryDrawer` mirrors this set in `FIELD_PASCAL_MAP`. When a user
saves a field, the backend appends its PascalCase name to the lock set
and enrichment passes skip it. When the user clicks "Revert to auto",
the component sends `revert_to_auto: [pascal]` in the PATCH body; the
backend NULLs the column and removes the name from the lock set so the
next enrichment pass can repopulate it.

The `StatusChip` reflects the presence of any lock (`operatorLocked`)
with a secondary badge — it does not enumerate which fields are locked.

## Location editing

Latitude and Longitude render as a single composite row inside the
entry drawer (`LocationSection`), backed by the reusable
`components/map/LocationPicker`. Clicks, drags, and the Clear button
flow through two paired `Controller`s so a single picker change flags
both `latitude` and `longitude` dirty. "Revert to auto" on the
Location row sends both `Latitude` and `Longitude` in
`revert_to_auto` and nulls both columns — the two halves always
travel together, matching the backend's paired-atomicity rule.

## Bulk metadata on Add IPs

`PasteStaging` exposes an optional "Default metadata (optional)"
disclosure above the invalid-tokens list. Operators can set
display name, city, country (`CountryPicker`), location
(`LocationPicker`), website, and notes once to apply the values to
every pasted IP. The panel starts collapsed; blank fields are
omitted from the wire body, and the disclosure's initial state
preserves the dialog's existing height layout.

On submit, `toMetadataWire` builds a `PasteRequest.metadata` block
that sends `country_code`+`country_name` atomically and
`latitude`+`longitude` atomically — half-filled pairs would return
400 `paired_metadata_half_missing` from the server. When the paste
response carries `skipped_summary.rows_with_skips > 0`, the
component renders a `role="status"` notice above the staged table
with the row count and the skipped-field label list; composite keys
(`Location`, `Country`) flow through to the UI verbatim.
