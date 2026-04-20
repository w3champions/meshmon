# `catalogue`

The authoritative per-IP registry. Every IP meshmon knows about —
operator-added or agent-derived — lives in `ip_catalogue`, with
per-row enrichment status, operator-overridable fields, and an SSE
event stream for live UI updates.

## Files

| File | Role |
|---|---|
| `mod.rs` | Module surface; re-exports the submodules below. |
| `model.rs` | `CatalogueEntry`, the `Field` enum, and the PascalCase encoding used in `operator_edited_fields`. |
| `parse.rs` | Paste tokeniser. Accepts bare IPv4 / IPv6 and `/32` or `/128` CIDRs; rejects wider prefixes. |
| `shapes.rs` | `Polygon(Vec<[f64; 2]>)` wire type and the point-in-polygon predicate used by the `shapes` filter. |
| `sort.rs` | `Cursor` (opaque base64 JSON), `SortBy`/`SortDir`, and the `CursorValueShape` gate that rejects typed-value mismatches at decode time. |
| `repo.rs` | sqlx queries: `insert_many`, `find_by_id`, `list` (keyset paging + sort + filters), `patch`, `delete`, `facets`, `ensure_from_agent`, `mark_enrichment_start`, `apply_enrichment_result`, `map_detail_or_clusters`. |
| `events.rs` | `CatalogueBroker` (wraps `tokio::sync::broadcast`, capacity 512) and the `CatalogueEvent` wire shape. |
| `sse.rs` | `GET /api/catalogue/stream` handler; serialises events and injects synthetic `lag` frames on receiver overflow. |
| `dto.rs` | `utoipa`-annotated request / response bodies — `ListQuery`, `ListResponse`, `MapQuery`, `MapResponse`, etc. |
| `handlers.rs` | HTTP handlers for paste, list, map, get, patch, delete, single / bulk re-enrich, and cached facets. |
| `facets.rs` | `FacetsCache`: 30-second TTL wrapper around `repo::facets` with single-flight refresh. |

## `GET /api/catalogue` — keyset paging, sort, filters

### Sort contract

`sort` picks the ordering column (`ip`, `display_name`, `city`,
`country_code`, `asn`, `network_operator`, `enrichment_status`,
`website`, `location`, `created_at`); `sort_dir` is `asc` or `desc`.
Both default to `created_at DESC` when unset.

Two invariants apply regardless of user input:

1. **`NULLS LAST` in every direction.** Columns that carry NULLs
   (display name, city, ASN, …) place them after every non-NULL row in
   both `ASC` and `DESC`. `location` is a derived boolean (`latitude IS
   NOT NULL AND longitude IS NOT NULL`) and follows the same rule.
2. **`id DESC` tiebreaker.** Every ordering appends `id DESC` so the
   keyset cursor is strictly decreasing within a column-value group.
   This is the cursor's correctness hinge: without it, rows with equal
   sort values could swap positions between pages.

### Cursor contract

Cursors are **opaque base64-encoded JSON** — clients must not inspect
or construct them. The decoder enforces three gates:

1. **Base64 shape.** Bad padding or non-alphabet characters surface as
   `CursorError::Base64`.
2. **JSON shape.** Structurally valid base64 that doesn't deserialise
   into the `Cursor` wire shape surfaces as `CursorError::Json`.
3. **Value-type match.** Each `SortBy` has a `cursor_value_shape`
   (`String` / `Number` / `Bool` / `Null`). A cursor whose JSON
   `value` is structurally valid but typed wrong — e.g. a `Number`
   value sent with `sort = display_name` — is rejected at decode time
   rather than silently yielding the wrong page.

On tamper, the handler **silently drops** the cursor and restarts from
the first page. This mirrors the `ip_prefix` filter's permissive-parse
posture: a malformed advisory parameter never breaks the request. The
trade-off is intentional — clients get a reset page instead of a 400
on a stored URL whose schema has drifted.

### Response shape

```json
{
  "entries": [...],
  "total": 327,
  "next_cursor": "eyJ2YWx1ZSI6...=="
}
```

- `entries` is the current page, ordered per the request's sort.
- `next_cursor` is `Some` when the server filled `limit` rows and a
  subsequent page may exist, `None` otherwise.
- `total` is the count of rows matching the filter, pre-page.

### `total` is approximate when shapes are active

When the `shapes` filter is non-empty, `total` is an **upper-bound
approximation**. The count query can only pre-filter by the union
bounding box of the shapes' rings — it cannot run the per-row
point-in-polygon test without materialising the whole filtered set.
Rows that land inside the bbox but outside every polygon are counted
in `total` while the page walk drops them from `entries`.

Clients that need the exact post-shape count must sum `entries.len()`
across every page.

## `GET /api/catalogue/map` — adaptive detail/cluster response

The map endpoint always scopes to a viewport (`bbox` is required; a
missing or malformed value is a 400). `sort`, `after`, `shapes`, and
`city` are intentionally out of the wire type — the map is paging-
free and shape-blind so operators can draw shapes against the
unfiltered fleet geography.

The response is one of two shapes, discriminated by `kind`:

- **`detail`** — raw rows — when the filtered viewport count is at or
  below `MAP_DETAIL_THRESHOLD` (2000).
- **`clusters`** — grid-aggregated buckets — when above the
  threshold. Cell size comes from `cell_size_for_zoom(zoom)` which
  bands zoom 0–20 into six steps (10° → 0.01°); zooms beyond 20 fall
  back to the finest band.

The bucket carries a sample catalogue id, a `lat`/`lng` centroid, a
count, and the bucket's own bbox — the frontend uses the bbox as the
`bbox` filter when it opens the cluster dialog.

## `POST /api/catalogue` — bulk metadata on paste

`PasteRequest` carries an optional `metadata` block applying one set
of operator values to every accepted IP. The handler routes through
`repo::insert_many_with_metadata`, which shares the same lock-aware
`CASE WHEN 'Field' = ANY(operator_edited_fields)` merge pattern as
`apply_enrichment_result`.

- **New rows** always receive every supplied field; the supplied
  field names are appended to `operator_edited_fields`.
- **Existing rows** receive a field only when it is not already in
  `operator_edited_fields`. Paired fields apply atomically: if either
  half of `CountryCode` + `CountryName` or `Latitude` + `Longitude`
  is locked, neither half is written and the skip log records the
  composite label (`"Country"` / `"Location"`).

The handler validates the same invariants as `PATCH` (finite lat/lon
in range, 2-char ASCII country code) plus the paste-specific paired-
presence rule — a half-supplied pair returns 400
`paired_metadata_half_missing` before touching the DB.

`PasteResponse.existing` reflects the post-merge state, so the UI
does not need a follow-up fetch. `PasteResponse.skipped_summary` is
present whenever the request carried `metadata` (absent otherwise)
and aggregates per-field skip counts with composite keys for paired
skips. An `Updated` event fires on the SSE broker for every existing
row whose merge actually wrote at least one column.
