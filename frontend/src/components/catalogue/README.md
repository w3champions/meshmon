# `components/catalogue`

UI components for the `/catalogue` page. They are composed inside the
catalogue route and share a single SSE connection opened at the page level.

## Files

| File | Role |
|---|---|
| `CatalogueTable.tsx` | `@tanstack/react-table` table with column-visibility toggle (persisted to `localStorage`). Rows are keyboard-accessible; clicking a row fires `onRowClick`. The per-row re-enrich button calls `onReenrich` without opening the drawer. |
| `CatalogueMap.tsx` | Thin adapter: converts `CatalogueEntry[]` into `DrawMapPin[]` (filtering out entries with no coordinates) and delegates rendering and shape management to `DrawMap`. Pin popups include an "Open details" link that fires `onRowClick`. |
| `EntryDrawer.tsx` | Right-side `Sheet` for editing a single entry. Uses `react-hook-form` + Zod; sends diff-only PATCH requests (only dirty fields are sent). Provides per-field "Revert to auto" links for operator-locked fields. Surfaces re-enrich and delete actions. |
| `PasteStaging.tsx` | Paste panel for bulk IP ingestion. Runs the client-side parser (`lib/catalogue-parse`) before POST so rejections surface immediately. After a successful POST, seeds the TanStack Query cache with server-returned entries and renders `StagingChip` per row, which reads enrichment status live from the cache as SSE events arrive. |
| `StatusChip.tsx` | Compact badge for the `enrichment_status` field (`pending`, `enriched`, `failed`). Appends an "Operator-edited" lock badge when `operatorLocked` is true. Optionally actionable (`onReenrich` prop) for `enriched` and `failed` states. |
| `ReenrichConfirm.tsx` | Modal confirmation dialog for bulk re-enrich. Parent owns the threshold logic (25-row gate); the dialog receives `selectionSize` and displays "~N ipgeolocation credits". |

## Composition inside `/catalogue`

The page holds shared state — active tab (table vs map), filter values,
shapes, and the selected entry id — and passes slices down:

```
CataloguePage
├── FilterRail          (filter state + shapes → server query params)
├── CatalogueTable      (visible when tab = "table")
├── CatalogueMap        (visible when tab = "map")
├── EntryDrawer         (open when selectedId !== undefined)
├── PasteStaging        (shown in a sheet/dialog driven by "Add IPs" button)
└── ReenrichConfirm     (open when bulk re-enrich selection ≥ 25)
```

The SSE singleton at the page level writes directly into the TanStack
Query cache. Components that display per-entry data read from
`['catalogue', 'entry', id]`; they do not open additional connections.

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
