import { useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  type CatalogueEntry,
  type CatalogueListQuery,
  type CatalogueMapQuery,
  type CatalogueSortBy,
  type CatalogueSortDir,
  useCatalogueEntry,
  useCatalogueFacets,
  useCatalogueListInfinite,
  useCatalogueMap,
  useReenrichMany,
  useReenrichOne,
} from "@/api/hooks/catalogue";
import { useCatalogueStream } from "@/api/hooks/catalogue-stream";
import { CatalogueClusterDialog } from "@/components/catalogue/CatalogueClusterDialog";
import { CatalogueMap } from "@/components/catalogue/CatalogueMap";
import { CatalogueTable, type CatalogueTableSort } from "@/components/catalogue/CatalogueTable";
import { EntryDrawer } from "@/components/catalogue/EntryDrawer";
import { PasteStaging } from "@/components/catalogue/PasteStaging";
import { ReenrichConfirm } from "@/components/catalogue/ReenrichConfirm";
import { FilterRail, type FilterValue } from "@/components/filter/FilterRail";
import { Button } from "@/components/ui/button";
import { type Bbox, type GeoShape, shapesToPolygons } from "@/lib/geo";
import { normalizeIpPrefix } from "@/lib/ip-prefix";
import { cn } from "@/lib/utils";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type ViewMode = "table" | "map";

interface CatalogueSearch {
  country?: string[];
  asn?: number[];
  network?: string[];
  city?: string[];
  ipPrefix?: string;
  name?: string;
  view?: ViewMode;
  sort?: CatalogueSortBy;
  dir?: CatalogueSortDir;
}

// ---------------------------------------------------------------------------
// URL ↔ FilterValue bridge
// ---------------------------------------------------------------------------

function filterFromSearch(search: CatalogueSearch): FilterValue {
  return {
    countryCodes: search.country ?? [],
    asns: search.asn ?? [],
    networks: search.network ?? [],
    cities: search.city ?? [],
    ipPrefix: search.ipPrefix,
    nameSearch: search.name,
    // shapes are never serialised into the URL — start empty every mount.
    shapes: [],
  };
}

function filterToSearch(value: FilterValue): Partial<CatalogueSearch> {
  const patch: Partial<CatalogueSearch> = {};
  if (value.countryCodes.length > 0) patch.country = value.countryCodes;
  else patch.country = undefined;
  if (value.asns.length > 0) patch.asn = value.asns;
  else patch.asn = undefined;
  if (value.networks.length > 0) patch.network = value.networks;
  else patch.network = undefined;
  if (value.cities.length > 0) patch.city = value.cities;
  else patch.city = undefined;
  patch.ipPrefix = value.ipPrefix ?? undefined;
  patch.name = value.nameSearch ?? undefined;
  return patch;
}

// ---------------------------------------------------------------------------
// ViewToggle
// ---------------------------------------------------------------------------

interface ViewToggleProps {
  value: ViewMode;
  onChange(next: ViewMode): void;
}

function ViewToggle({ value, onChange }: ViewToggleProps) {
  return (
    <fieldset className="flex gap-1 border-0 p-0 m-0">
      <legend className="sr-only">View mode</legend>
      <Button
        type="button"
        size="sm"
        variant={value === "table" ? "default" : "outline"}
        aria-pressed={value === "table"}
        aria-label="Table view"
        onClick={() => onChange("table")}
      >
        Table
      </Button>
      <Button
        type="button"
        size="sm"
        variant={value === "map" ? "default" : "outline"}
        aria-pressed={value === "map"}
        aria-label="Map view"
        onClick={() => onChange("map")}
      >
        Map
      </Button>
    </fieldset>
  );
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function Catalogue() {
  // Mount the SSE stream once for the lifetime of this page. The stream
  // invalidates the list, map, and facets caches on every catalogue
  // event so server-driven queries pick up inserts/deletes/updates.
  useCatalogueStream();

  const rawSearch = useSearch({ strict: false }) as CatalogueSearch;
  const navigate = useNavigate();

  // Derive FilterValue from URL search params (shapes are always local state).
  const [shapes, setShapes] = useState<GeoShape[]>([]);
  const filterFromUrl = useMemo(() => filterFromSearch(rawSearch), [rawSearch]);

  // Merge URL-driven fields with local shapes so the rest of the page can use
  // a single FilterValue object.
  const filter: FilterValue = useMemo(
    () => ({ ...filterFromUrl, shapes }),
    [filterFromUrl, shapes],
  );

  const view: ViewMode = rawSearch.view ?? "table";

  // Sort state lives in the URL alongside the filter facets so operators
  // can share a table view by URL. `col`/`dir` are both nullable — unset
  // means "unsorted" and the server falls back to `created_at DESC`.
  const sort: CatalogueTableSort = useMemo(
    () => ({
      col: rawSearch.sort ?? null,
      dir: rawSearch.dir ?? null,
    }),
    [rawSearch.sort, rawSearch.dir],
  );

  // Synchronise filter changes back to the URL (shapes excluded).
  // Stable across renders so it can be passed to memoized children
  // (FilterRail, CatalogueMap) without defeating their memoization.
  const setFilter = useCallback(
    (next: FilterValue): void => {
      setShapes(next.shapes);
      const searchUpdate = {
        ...filterToSearch(next),
        view: rawSearch.view,
        sort: rawSearch.sort,
        dir: rawSearch.dir,
      } as CatalogueSearch;
      // Cast required: route is not yet registered in the router type tree
      // (Task 16 wires it); strict:false means the router sees `never` for search.
      void (navigate as (opts: { search: unknown; replace: boolean }) => void)({
        search: searchUpdate,
        replace: true,
      });
    },
    [navigate, rawSearch.view, rawSearch.sort, rawSearch.dir],
  );

  const setView = useCallback(
    (next: ViewMode): void => {
      const searchUpdate: CatalogueSearch = {
        ...rawSearch,
        view: next,
      };
      void (navigate as (opts: { search: unknown; replace: boolean }) => void)({
        search: searchUpdate,
        replace: true,
      });
    },
    [navigate, rawSearch],
  );

  const setSort = useCallback(
    (col: CatalogueSortBy | null, dir: CatalogueSortDir | null): void => {
      const searchUpdate = {
        ...rawSearch,
        sort: col ?? undefined,
        dir: dir ?? undefined,
      } as CatalogueSearch;
      void (navigate as (opts: { search: unknown; replace: boolean }) => void)({
        search: searchUpdate,
        replace: true,
      });
    },
    [navigate, rawSearch],
  );

  // Drawer state. `drawerSeedEntry` is populated when the drawer is
  // opened for a row that isn't in the main table's loaded pages — for
  // example a cluster-dialog hit further down the feed. It seeds the
  // drawer so we don't flash an empty dialog while the per-entry query
  // catches up.
  const [drawerId, setDrawerId] = useState<string | null>(null);
  const [drawerSeedEntry, setDrawerSeedEntry] = useState<CatalogueEntry | null>(null);

  // Paste panel state
  const [pasteOpen, setPasteOpen] = useState(false);

  // Bulk re-enrich confirm state
  const [reenrichConfirmOpen, setReenrichConfirmOpen] = useState(false);

  // Cluster dialog state — holds the bbox of the clicked cluster cell
  // or `null` while the dialog is closed. The dialog owns its own
  // `useCatalogueListInfinite` query scoped to this bbox.
  const [clusterCell, setClusterCell] = useState<Bbox | null>(null);

  // Map viewport state — seeded on the map's first `moveend` (which
  // `ViewportController` publishes on mount). `useCatalogueMap` stays
  // disabled until this is set so a just-mounted map doesn't race.
  const [mapViewport, setMapViewport] = useState<{ bbox: Bbox; zoom: number } | null>(null);

  // -----------------------------------------------------------------------
  // Server-driven table query. Runs every filter facet — including `city`
  // (ANY semantics, backend-native) and `shapes` (serialised as a JSON
  // array of `[lng, lat]` rings, point-in-polygon matched server-side).
  // -----------------------------------------------------------------------
  const tableQuery: CatalogueListQuery = useMemo(() => {
    const q: CatalogueListQuery = {};
    if (filter.countryCodes.length > 0) q.country_code = filter.countryCodes;
    if (filter.asns.length > 0) q.asn = filter.asns;
    if (filter.networks.length > 0) q.network = filter.networks;
    if (filter.cities.length > 0) q.city = filter.cities;
    if (filter.ipPrefix) {
      // Backend accepts only valid CIDR; expand bare dotted prefixes so
      // natural operator input (`10.0.0.`) doesn't silently match everything.
      const normalized = normalizeIpPrefix(filter.ipPrefix);
      if (normalized) q.ip_prefix = normalized;
    }
    if (filter.nameSearch) q.name = filter.nameSearch;
    if (filter.shapes.length > 0) {
      q.shapes = JSON.stringify(shapesToPolygons(filter.shapes));
    }
    if (sort.col) q.sort = sort.col;
    if (sort.dir) q.sort_dir = sort.dir;
    return q;
  }, [filter, sort.col, sort.dir]);

  const tableInfinite = useCatalogueListInfinite(tableQuery);
  const rows: CatalogueEntry[] = useMemo(
    () => tableInfinite.data?.pages.flatMap((p) => p.entries) ?? [],
    [tableInfinite.data],
  );
  const total = tableInfinite.data?.pages[0]?.total ?? 0;

  // -----------------------------------------------------------------------
  // Server-driven map query. Intentionally drops `city`, `shapes`, and
  // sort — the map endpoint's wire type (`MapQuery`) doesn't carry any
  // of them. Operators draw shapes against the unfiltered fleet geography.
  // -----------------------------------------------------------------------
  const mapQuery: CatalogueMapQuery = useMemo(() => {
    const q: CatalogueMapQuery = {};
    if (filter.countryCodes.length > 0) q.country_code = filter.countryCodes;
    if (filter.asns.length > 0) q.asn = filter.asns;
    if (filter.networks.length > 0) q.network = filter.networks;
    if (filter.ipPrefix) {
      const normalized = normalizeIpPrefix(filter.ipPrefix);
      if (normalized) q.ip_prefix = normalized;
    }
    if (filter.nameSearch) q.name = filter.nameSearch;
    return q;
  }, [filter.countryCodes, filter.asns, filter.networks, filter.ipPrefix, filter.nameSearch]);

  const mapInfinite = useCatalogueMap(mapViewport?.bbox, mapViewport?.zoom ?? 2, mapQuery);

  // -----------------------------------------------------------------------
  // Cluster dialog filters: must match the map endpoint's filter set
  // exactly, otherwise "N in this area" on the cluster bubble
  // disagrees with the dialog body's row count whenever a shape- or
  // city-filter is active. Drop both `shapes` and `city` — the same
  // two keys `mapQuery` drops — so country/ASN/network/ip_prefix/name
  // propagate but the viewport semantics stay consistent across the
  // bubble → dialog handoff. Sort still rides along so a pre-sorted
  // table view surfaces cluster contents in the same order.
  // -----------------------------------------------------------------------
  const dialogFilters: CatalogueListQuery = useMemo(() => {
    const { shapes: _shapes, city: _city, ...rest } = tableQuery;
    return rest;
  }, [tableQuery]);

  const { data: facetsData } = useCatalogueFacets();
  const reenrichOneMutation = useReenrichOne();
  const reenrichManyMutation = useReenrichMany();

  // The drawer needs the full entry object, not just an id. We look it
  // up first in the main table's loaded pages; if it isn't there — for
  // example when the operator opened the drawer from a cluster dialog
  // hit that falls outside the main table's filter — we fall back to
  // `drawerSeedEntry`, but ONLY when the seed's id matches the current
  // `drawerId`. Without the id gate, this sequence would leak:
  //   1. open cluster entry A  → drawerId=A, seed=A
  //   2. click table row B     → drawerId=B, seed stays A (table clicks
  //      don't touch the seed path since table rows are already in
  //      `rows`)
  //   3. filter change drops B out of `rows`
  //   → fallback would surface the stale A seed for drawer B.
  const drawerEntry = useMemo(() => {
    if (drawerId === null) return undefined;
    const fromRows = rows.find((e) => e.id === drawerId);
    if (fromRows) return fromRows;
    return drawerSeedEntry?.id === drawerId ? drawerSeedEntry : undefined;
  }, [rows, drawerId, drawerSeedEntry]);

  // Drawer deletion guard: if the list refetches (e.g. after an SSE
  // `deleted` event) and the open entry is gone from every loaded page
  // AND the seed doesn't cover it (same id gate as `drawerEntry`),
  // clear `drawerId` so the drawer state stays consistent. Cluster-
  // dialog-opened entries stay alive via the seed — they're
  // legitimately outside `rows` and must not be closed by THIS guard.
  useEffect(() => {
    if (
      drawerId !== null &&
      tableInfinite.data !== undefined &&
      !rows.some((e) => e.id === drawerId) &&
      drawerSeedEntry?.id !== drawerId
    ) {
      setDrawerId(null);
    }
  }, [drawerId, rows, tableInfinite.data, drawerSeedEntry]);

  // Server-side deletion guard: subscribes to the per-entry endpoint
  // while the drawer is open. The SSE `deleted` handler invokes
  // `removeQueries` on this key, which triggers a refetch → 404 →
  // `data === null`. That's the authoritative "row is gone
  // server-side" signal and closes the drawer regardless of how it
  // was opened (rows-path OR seed-path). Without this, a cluster-
  // dialog-opened entry that another operator deletes would stay in
  // the drawer indefinitely because the rows-based guard above is
  // deliberately shielded by the seed.
  const drawerEntryQuery = useCatalogueEntry(drawerId ?? undefined);
  useEffect(() => {
    if (drawerId !== null && drawerEntryQuery.data === null) {
      setDrawerId(null);
      setDrawerSeedEntry(null);
    }
  }, [drawerId, drawerEntryQuery.data]);

  const handleReenrichOne = useCallback(
    (id: string): void => {
      reenrichOneMutation.mutate(id);
    },
    [reenrichOneMutation],
  );

  // Bulk re-enrich fires against currently-loaded rows only; the button
  // label below reflects the loaded subset whenever pagination isn't
  // exhausted so the action matches what the operator sees.
  const handleReenrichMany = useCallback((): void => {
    const ids = rows.map((e) => e.id);
    reenrichManyMutation.mutate({ ids });
    setReenrichConfirmOpen(false);
  }, [rows, reenrichManyMutation]);

  // Stable row-click & shape-change handlers for memoized heavy children.
  const handleRowClick = useCallback((id: string): void => {
    setDrawerId(id);
  }, []);

  const handleShapesChange = useCallback(
    (nextShapes: GeoShape[]): void => {
      setFilter({ ...filter, shapes: nextShapes });
    },
    [filter, setFilter],
  );

  const handleViewportChange = useCallback((bbox: Bbox, zoom: number): void => {
    setMapViewport({ bbox, zoom });
  }, []);

  const handleClusterOpen = useCallback((cell: Bbox): void => {
    setClusterCell(cell);
  }, []);

  const handleClusterDialogOpenChange = useCallback((open: boolean): void => {
    if (!open) setClusterCell(null);
  }, []);

  // Opens the drawer with a full entry seed. Used by both the cluster
  // dialog (rows scoped by cell bbox, not in the main table's pages)
  // and the map detail popup (map omits `city`/`shapes` filters, so a
  // visible pin may represent a row the table didn't load). Seeding
  // lets `drawerEntry` fall back gracefully when `rows.find(...)`
  // misses, and also suppresses the deletion-guard early-close.
  const handleOpenEntryWithSeed = useCallback((entry: CatalogueEntry): void => {
    setClusterCell(null);
    setDrawerSeedEntry(entry);
    setDrawerId(entry.id);
  }, []);

  const handleDrawerClose = useCallback((): void => {
    setDrawerId(null);
    setDrawerSeedEntry(null);
  }, []);

  return (
    <div className="flex h-full overflow-hidden">
      {/* Filter rail */}
      <aside className="w-64 shrink-0 overflow-y-auto border-r p-3">
        <FilterRail
          value={filter}
          onChange={setFilter}
          facets={facetsData}
          onOpenMap={() => setView("map")}
        />
      </aside>

      {/* Main content */}
      <div className="flex flex-1 flex-col overflow-hidden">
        <header className="flex items-center gap-2 border-b px-4 py-3">
          <ViewToggle value={view} onChange={setView} />
          <div className="ml-auto flex items-center gap-2">
            <Button type="button" size="sm" onClick={() => setPasteOpen(true)}>
              Add IPs
            </Button>
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={rows.length === 0}
              onClick={() => setReenrichConfirmOpen(true)}
            >
              {rows.length < total
                ? `Re-enrich loaded (${rows.length} of ${total})`
                : `Re-enrich all (${total})`}
            </Button>
          </div>
        </header>

        <main className="flex-1 overflow-auto p-4">
          {view === "table" ? (
            <CatalogueTable
              rows={rows}
              total={total}
              hasNextPage={tableInfinite.hasNextPage}
              isFetchingNextPage={tableInfinite.isFetchingNextPage}
              fetchNextPage={tableInfinite.fetchNextPage}
              sort={sort}
              onSortChange={setSort}
              onRowClick={handleRowClick}
              onReenrich={handleReenrichOne}
            />
          ) : (
            <CatalogueMap
              response={mapInfinite.data}
              isLoading={mapInfinite.isLoading}
              isError={mapInfinite.isError}
              shapes={filter.shapes}
              onShapesChange={handleShapesChange}
              onOpenEntry={handleOpenEntryWithSeed}
              onClusterOpen={handleClusterOpen}
              onViewportChange={handleViewportChange}
              className={cn("h-full w-full")}
            />
          )}
        </main>
      </div>

      {/* Entry drawer — passes undefined to close */}
      <EntryDrawer entry={drawerEntry} onClose={handleDrawerClose} />

      {/* Paste panel */}
      <PasteStaging open={pasteOpen} onOpenChange={(next) => setPasteOpen(next)} />

      {/* Cluster dialog — owns its own infinite query scoped to the cell */}
      <CatalogueClusterDialog
        open={clusterCell !== null}
        onOpenChange={handleClusterDialogOpenChange}
        cell={clusterCell}
        filters={dialogFilters}
        onOpenEntry={handleOpenEntryWithSeed}
      />

      {/* Bulk re-enrich confirm */}
      <ReenrichConfirm
        selectionSize={rows.length}
        open={reenrichConfirmOpen}
        onConfirm={handleReenrichMany}
        onCancel={() => setReenrichConfirmOpen(false)}
      />
    </div>
  );
}
