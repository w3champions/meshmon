import { useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  type CatalogueEntry,
  type CatalogueListQuery,
  useCatalogueFacets,
  useCatalogueList,
  useReenrichMany,
  useReenrichOne,
} from "@/api/hooks/catalogue";
import { useCatalogueStream } from "@/api/hooks/catalogue-stream";
import { CatalogueMap } from "@/components/catalogue/CatalogueMap";
import { CatalogueTable } from "@/components/catalogue/CatalogueTable";
import { EntryDrawer } from "@/components/catalogue/EntryDrawer";
import { PasteStaging } from "@/components/catalogue/PasteStaging";
import { ReenrichConfirm } from "@/components/catalogue/ReenrichConfirm";
import { FilterRail, type FilterValue } from "@/components/filter/FilterRail";
import { Button } from "@/components/ui/button";
import { type GeoShape, pointInShapes } from "@/lib/geo";
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
// Filter → backend query params
// API only supports: country_code, asn, network, ip_prefix, name, bbox
// city is client-side only; shapes may produce a bbox pre-filter
// ---------------------------------------------------------------------------

function buildQuery(filter: FilterValue): CatalogueListQuery {
  const q: CatalogueListQuery = {};
  if (filter.countryCodes.length > 0) q.country_code = filter.countryCodes;
  if (filter.asns.length > 0) q.asn = filter.asns;
  if (filter.networks.length > 0) q.network = filter.networks;
  if (filter.ipPrefix) q.ip_prefix = filter.ipPrefix;
  if (filter.nameSearch) q.name = filter.nameSearch;
  return q;
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
  // Mount the SSE stream once for the lifetime of this page.
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

  // Synchronise filter changes back to the URL (shapes excluded).
  // Stable across renders so it can be passed to memoized children
  // (FilterRail, CatalogueMap) without defeating their memoization.
  const setFilter = useCallback(
    (next: FilterValue): void => {
      setShapes(next.shapes);
      const searchUpdate = {
        ...filterToSearch(next),
        view: rawSearch.view,
      } as CatalogueSearch;
      // Cast required: route is not yet registered in the router type tree
      // (Task 16 wires it); strict:false means the router sees `never` for search.
      void (navigate as (opts: { search: unknown; replace: boolean }) => void)({
        search: searchUpdate,
        replace: true,
      });
    },
    [navigate, rawSearch.view],
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

  // Drawer state
  const [drawerId, setDrawerId] = useState<string | null>(null);

  // Paste panel state
  const [pasteOpen, setPasteOpen] = useState(false);

  // Bulk re-enrich confirm state
  const [reenrichConfirmOpen, setReenrichConfirmOpen] = useState(false);

  // Data hooks
  const query = useMemo(() => buildQuery(filter), [filter]);
  const { data: listData } = useCatalogueList(query);
  const { data: facetsData } = useCatalogueFacets();
  const reenrichOneMutation = useReenrichOne();
  const reenrichManyMutation = useReenrichMany();

  const allEntries: CatalogueEntry[] = listData?.entries ?? [];

  // Client-side city filter (not supported by the backend).
  const afterCityFilter = useMemo(() => {
    if (filter.cities.length === 0) return allEntries;
    return allEntries.filter((e) => e.city != null && filter.cities.includes(e.city));
  }, [allEntries, filter.cities]);

  // Client-side shape filter layered on top of everything else.
  const visibleEntries = useMemo(() => {
    if (filter.shapes.length === 0) return afterCityFilter;
    return afterCityFilter.filter((e) => {
      if (e.latitude == null || e.longitude == null) return false;
      return pointInShapes(e.latitude, e.longitude, filter.shapes);
    });
  }, [afterCityFilter, filter.shapes]);

  // The drawer needs the full entry object, not just an id.
  const drawerEntry = useMemo(
    () => allEntries.find((e) => e.id === drawerId),
    [allEntries, drawerId],
  );

  // Drawer deletion guard: if the list refetches (e.g. after an SSE `deleted`
  // event) and the open entry is gone, clear `drawerId` so the drawer state
  // stays consistent. Without this, `drawerId` lingers and re-opening the
  // same id later would briefly flash stale behaviour.
  useEffect(() => {
    if (drawerId !== null && listData !== undefined && drawerEntry === undefined) {
      setDrawerId(null);
    }
  }, [drawerId, drawerEntry, listData]);

  // Stable across renders. `useReenrichOne` / `useReenrichMany` return
  // reference-stable mutation objects so capturing `.mutate` directly would
  // work, but passing the mutation object keeps the dep explicit.
  const handleReenrichOne = useCallback(
    (id: string): void => {
      reenrichOneMutation.mutate(id);
    },
    [reenrichOneMutation],
  );

  const handleReenrichMany = useCallback((): void => {
    const ids = visibleEntries.map((e) => e.id);
    reenrichManyMutation.mutate({ ids });
    setReenrichConfirmOpen(false);
  }, [visibleEntries, reenrichManyMutation]);

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

  const handleDrawerClose = useCallback((): void => {
    setDrawerId(null);
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
              disabled={visibleEntries.length === 0}
              onClick={() => setReenrichConfirmOpen(true)}
            >
              Re-enrich all ({visibleEntries.length})
            </Button>
          </div>
        </header>

        <main className="flex-1 overflow-auto p-4">
          {view === "table" ? (
            <CatalogueTable
              entries={visibleEntries}
              onRowClick={handleRowClick}
              onReenrich={handleReenrichOne}
            />
          ) : (
            <CatalogueMap
              entries={visibleEntries}
              shapes={filter.shapes}
              onShapesChange={handleShapesChange}
              onRowClick={handleRowClick}
              className={cn("h-full w-full")}
            />
          )}
        </main>
      </div>

      {/* Entry drawer — passes undefined to close */}
      <EntryDrawer entry={drawerEntry} onClose={handleDrawerClose} />

      {/* Paste panel */}
      {pasteOpen && <PasteStaging onClose={() => setPasteOpen(false)} />}

      {/* Bulk re-enrich confirm */}
      <ReenrichConfirm
        selectionSize={visibleEntries.length}
        open={reenrichConfirmOpen}
        onConfirm={handleReenrichMany}
        onCancel={() => setReenrichConfirmOpen(false)}
      />
    </div>
  );
}
