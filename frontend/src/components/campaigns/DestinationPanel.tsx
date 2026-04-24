import { useVirtualizer } from "@tanstack/react-virtual";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { useCatalogueListInfinite } from "@/api/hooks/catalogue";
import type { components } from "@/api/schema.gen";
import { PasteStaging } from "@/components/catalogue/PasteStaging";
import { FilterRail, type FilterValue } from "@/components/filter/FilterRail";
import { IpHostname } from "@/components/ip-hostname";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { destinationFilterToQuery } from "@/lib/catalogue-query";
import { lookupCountryName } from "@/lib/countries";
import { cn } from "@/lib/utils";
import { useToastStore } from "@/stores/toast";

type FacetsResponse = components["schemas"]["FacetsResponse"];

const ROW_HEIGHT = 44;
const SCROLL_MAX_HEIGHT = "60vh";
const INITIAL_SCROLL_RECT = { width: 1024, height: 600 };

/**
 * Catalogue page size. Matches the server-side `1..=500` clamp so the
 * panel issues one fetch per batch of destinations that crosses the
 * wire. The exhaustive "Add all" walk (`handleAddAllExhaustive`) drains
 * subsequent pages through the same hook, so a single page size serves
 * both browse and walk flows.
 */
const PAGE_SIZE = 500;

/**
 * Shared CSS grid track expression for the header + virtualized body.
 * Columns: IP, Name, City, Country, ASN, Network, selected-check.
 *
 * The IP track widens to `minmax(180px, 1.3fr)` so that `<IpHostname>`'s
 * `ip (hostname)` rendering has room before the cell clips — a bare IP
 * fits in 120px, but `203.0.113.10 (mail.example.com)` is a comfortable
 * 180px floor on typical hostnames.
 */
const GRID_TEMPLATE =
  "minmax(180px, 1.3fr) minmax(140px, 1.2fr) minmax(100px, 1fr) minmax(110px, 1fr) 80px minmax(140px, 1.5fr) 32px";

export interface DestinationPanelProps {
  selected: Set<string>;
  onSelectedChange(next: Set<string>): void;
  filter: FilterValue;
  onFilterChange(next: FilterValue): void;
  facets: FacetsResponse | undefined;
  onOpenMap(): void;
  /**
   * When true, every mutation button (Add all / Remove all / Add IPs /
   * Map view) is disabled and row-click no-ops. Filter rail stays live
   * so the operator can still inspect matches. Composer sets this after
   * a draft campaign has been created so further destination edits
   * don't silently diverge from the persisted draft.
   */
  disabled?: boolean;
}

export function DestinationPanel({
  selected,
  onSelectedChange,
  filter,
  onFilterChange,
  facets,
  onOpenMap,
  disabled = false,
}: DestinationPanelProps) {
  const query = useMemo(() => destinationFilterToQuery(filter), [filter]);
  const listQuery = useCatalogueListInfinite(query, { pageSize: PAGE_SIZE });
  const [pasteOpen, setPasteOpen] = useState(false);

  // Mirror the live `query` and `selected` props into refs so the walk
  // handler — whose closure is frozen at click time — can observe
  // mid-walk mutations. `useLayoutEffect` runs synchronously after
  // commit, before any microtask can drain; plain `useEffect` is
  // deferred until after paint, which leaves a window where a
  // post-commit resolved Promise could still read the old value.
  const queryRef = useRef(query);
  useLayoutEffect(() => {
    queryRef.current = query;
    // A prior walk's error belonged to a previous filter and has no
    // meaning under the new one — clear the banner on every filter
    // change.
    setWalkError(null);
  }, [query]);

  const selectedRef = useRef(selected);
  useLayoutEffect(() => {
    selectedRef.current = selected;
  }, [selected]);

  const rows: CatalogueEntry[] = useMemo(
    () => listQuery.data?.pages.flatMap((p) => p.entries) ?? [],
    [listQuery.data],
  );

  const total = listQuery.data?.pages[0]?.total ?? 0;
  const shapesActive = filter.shapes.length > 0;

  // --- Exhaustive-walk state -------------------------------------------
  // "Add all" drains the cursor chain under the current filter and merges
  // every returned IP into `selected`. The walk owns state private to
  // the panel — the composer does not need to know about progress.
  const [walkProgress, setWalkProgress] = useState<{ collected: number; total: number } | null>(
    null,
  );
  const [walkError, setWalkError] = useState<Error | null>(null);
  // Ref (not state) so rapid double-clicks are guarded in the same tick.
  const walkRunningRef = useRef(false);

  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 8,
    initialRect: INITIAL_SCROLL_RECT,
  });

  const addIps = (ips: Iterable<string>) => {
    if (disabled) return;
    const next = new Set(selected);
    for (const ip of ips) next.add(ip);
    onSelectedChange(next);
  };

  const handleAddAllExhaustive = async () => {
    if (disabled || walkRunningRef.current) return;

    // Guard against a pre-first-page click. Seeding from an empty
    // snapshot would emit `collectedIps.size === 0` and bail into the
    // "no matching rows" path — that toast is correct but misleading
    // when the real state is "catalogue hasn't returned yet". Nudge the
    // operator to retry once the first page lands.
    const initialPages = listQuery.data?.pages ?? [];
    if (initialPages.length === 0) {
      useToastStore.getState().pushToast({
        kind: "error",
        message: "Catalogue still loading — try again in a moment.",
      });
      return;
    }

    // Snapshot the active filter query. If the operator mutates the
    // filter mid-walk, the infinite query key changes and subsequent
    // `fetchNextPage` calls route to a different cursor chain — the
    // aggregation would Frankenstein-union pages from two filters. The
    // FilterRail stays live during the walk on purpose, so this check
    // is the only defence against that race. Read via the ref — the
    // handler closure is frozen at click time so a stale `query`
    // wouldn't detect the change.
    const startKey = JSON.stringify(queryRef.current);

    walkRunningRef.current = true;
    setWalkError(null);

    try {
      const seedTotal = initialPages[0]?.total ?? 0;
      let collectedCount = initialPages.reduce((n, p) => n + p.entries.length, 0);
      setWalkProgress({ collected: collectedCount, total: seedTotal });

      // Drain remaining pages. `throwOnError: true` is load-bearing —
      // without it, TanStack Query's `fetchNextPage` resolves with an
      // error-carrying result (never rejects), so the walk would spin
      // on a page-fetch failure with `result.hasNextPage` still true.
      let hasNext = listQuery.hasNextPage;
      let latestPages = initialPages;
      while (hasNext) {
        const result = await listQuery.fetchNextPage({ throwOnError: true });

        // Filter changed since walk start — abort instead of mixing
        // pages from different queries.
        if (JSON.stringify(queryRef.current) !== startKey) {
          useToastStore.getState().pushToast({
            kind: "error",
            message: "Filter changed — Add all aborted. Re-click to rerun under the new filter.",
          });
          setWalkProgress(null);
          return;
        }

        latestPages = result.data?.pages ?? latestPages;
        collectedCount = latestPages.reduce((n, p) => n + p.entries.length, 0);
        const pageTotal = latestPages[0]?.total ?? seedTotal;
        setWalkProgress({ collected: collectedCount, total: pageTotal });
        hasNext = result.hasNextPage ?? false;
      }

      const collectedIps = new Set<string>();
      for (const page of latestPages) {
        for (const entry of page.entries) collectedIps.add(entry.ip);
      }

      // Defence-in-depth: if the walk aggregated zero IPs (pages arrived
      // but carried no entries), don't emit a no-op set-swap.
      if (collectedIps.size === 0) {
        useToastStore.getState().pushToast({
          kind: "error",
          message: "Catalogue returned no matching rows — selection left unchanged.",
        });
        setWalkProgress(null);
        return;
      }

      // Additive merge — prior manual row-clicks and IPs from a
      // narrower filter survive a subsequent "Add all" pass. Read via
      // `selectedRef` so row-click toggles that landed mid-walk are
      // preserved; the closure-captured `selected` is frozen at click
      // time and would silently clobber those edits.
      const merged = new Set(selectedRef.current);
      for (const ip of collectedIps) merged.add(ip);
      onSelectedChange(merged);
      setWalkProgress(null);
    } catch (err) {
      const error = err instanceof Error ? err : new Error(String(err));
      setWalkError(error);
      useToastStore.getState().pushToast({ kind: "error", message: error.message });
      setWalkProgress(null);
    } finally {
      walkRunningRef.current = false;
    }
  };

  const handleRemoveAll = () => {
    if (disabled) return;
    if (selected.size === 0) return;
    onSelectedChange(new Set());
  };

  const handleToggleRow = (ip: string) => {
    if (disabled) return;
    const next = new Set(selected);
    if (next.has(ip)) next.delete(ip);
    else next.add(ip);
    onSelectedChange(next);
  };

  const handlePasteSuccess = (ips: string[]) => {
    // Every acknowledged IP — both `created` and `existing` buckets —
    // feeds into the selection set so operators don't have to re-add
    // rows they just pasted.
    addIps(ips);
    setPasteOpen(false);
  };

  const estimatedTotal = shapesActive ? `~${total}` : `${total}`;

  const walking = walkProgress !== null;
  // "Add all" re-uses the display hook, so clicking before the first
  // page lands races the empty-snapshot guard above. Gate on
  // `isLoading` for the no-cache first-fetch case and on
  // `pagesLoaded === 0` for the post-filter-change refetch case.
  //
  // Gate on pages-loaded, NOT on `rows.length`: shape filters apply
  // point-in-polygon AFTER the SQL-bbox-limited page, so a first page
  // can arrive with `entries: []` and `next_cursor != null` — the
  // walk still has work to do. Gating on empty entries would lock the
  // button for those filters even though later cursor pages may match.
  //
  // Do NOT gate on `isFetching` — the campaign-stream subscription
  // triggers background refetches of the same query on every
  // lifecycle event, which would flicker the button on a stable
  // filter.
  const pagesLoaded = listQuery.data?.pages.length ?? 0;
  const addAllDisabled = disabled || walking || listQuery.isLoading || pagesLoaded === 0;

  return (
    <section aria-label="Destinations" className="flex h-full min-h-0 gap-3">
      <aside className="w-64 shrink-0 overflow-y-auto">
        <FilterRail
          value={filter}
          onChange={onFilterChange}
          facets={facets}
          onOpenMap={onOpenMap}
        />
      </aside>

      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <header className="flex flex-wrap items-center gap-2">
          <h2 className="text-base font-semibold">Destinations</h2>
          <Badge variant="secondary" aria-label="Selected destinations count">
            {selected.size} destinations selected
          </Badge>
          <div className="ml-auto flex items-center gap-2">
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={handleAddAllExhaustive}
              disabled={addAllDisabled}
            >
              {walking ? `Adding ${walkProgress.collected}…` : "Add all"}
            </Button>
            <Button
              type="button"
              size="sm"
              variant="ghost"
              onClick={handleRemoveAll}
              disabled={disabled || selected.size === 0}
            >
              Remove all
            </Button>
            <Button type="button" size="sm" onClick={() => setPasteOpen(true)} disabled={disabled}>
              Add IPs
            </Button>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={onOpenMap}
              disabled={disabled}
            >
              Map view
            </Button>
          </div>
        </header>

        {walkProgress !== null ? (
          <p
            role="status"
            aria-live="polite"
            className="rounded-md border border-dashed bg-muted/30 px-3 py-2 text-sm"
          >
            Collecting {walkProgress.collected} of {shapesActive ? "~" : ""}
            {walkProgress.total} destinations…
          </p>
        ) : null}

        {walkError !== null ? (
          <div
            role="alert"
            className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            Walk failed: {walkError.message}
          </div>
        ) : null}

        {/*
          Real `<table>` can't carry `fr` tracks (table-layout needs
          absolute widths) and would clip-past-right-edge on narrow
          viewports. We mirror CatalogueTable's div+grid pattern — ARIA
          roles preserve screen-reader semantics while CSS grid owns
          layout.
        */}
        {/* biome-ignore lint/a11y/useSemanticElements: see block comment above — a real <table> would force `table-layout: fixed` and break `fr` tracks. */}
        <div role="table" aria-label="Destinations" className="flex flex-col rounded-md border">
          {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
          {/* biome-ignore lint/a11y/useFocusableInteractive: role="row" is a grouping role in the ARIA table pattern — not an interactive control. */}
          <div
            role="row"
            className="grid w-full border-b bg-muted/30 text-sm font-medium text-muted-foreground"
            style={{ gridTemplateColumns: GRID_TEMPLATE }}
          >
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              IP
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              Name
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              City
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              Country
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              ASN
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              Network
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              &nbsp;
            </div>
          </div>

          <div
            ref={scrollRef}
            className="relative overflow-auto"
            style={{ maxHeight: SCROLL_MAX_HEIGHT }}
          >
            <div style={{ position: "relative", height: `${virtualizer.getTotalSize()}px` }}>
              {virtualizer.getVirtualItems().map((item) => {
                const entry = rows[item.index];
                if (!entry) return null;
                const country =
                  lookupCountryName(entry.country_code ?? undefined) ?? entry.country_code ?? "—";
                const isSelected = selected.has(entry.ip);
                return (
                  /* biome-ignore lint/a11y/useSemanticElements: virtualized row is a CSS-grid parent; role="button" keeps the click+keyboard affordance. */
                  <div
                    key={entry.id}
                    role="button"
                    tabIndex={0}
                    aria-pressed={isSelected}
                    aria-label={`Toggle destination ${entry.ip}`}
                    onClick={() => handleToggleRow(entry.ip)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        handleToggleRow(entry.ip);
                      }
                    }}
                    className={cn(
                      "absolute top-0 left-0 grid w-full cursor-pointer items-center overflow-hidden border-b text-sm hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none",
                      isSelected && "bg-primary/10",
                    )}
                    style={{
                      transform: `translateY(${item.start}px)`,
                      height: `${item.size}px`,
                      gridTemplateColumns: GRID_TEMPLATE,
                    }}
                  >
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3 text-xs">
                      <IpHostname ip={entry.ip} />
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3">
                      {entry.display_name ?? "—"}
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3">
                      {entry.city ?? "—"}
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3">
                      {country}
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3">
                      {entry.asn != null ? String(entry.asn) : "—"}
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3">
                      {entry.network_operator ?? "—"}
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="px-3 text-muted-foreground">
                      {isSelected ? "✓" : ""}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>

          {rows.length === 0 ? (
            <p className="px-4 py-6 text-center text-sm text-muted-foreground">
              No catalogue rows match the current filter.
            </p>
          ) : null}

          <footer className="flex items-center justify-between gap-3 border-t bg-muted/20 px-4 py-2 text-xs text-muted-foreground">
            <span>{rows.length} loaded</span>
            <span>Estimated total: {estimatedTotal}</span>
          </footer>
        </div>
      </div>

      <PasteStaging
        open={pasteOpen}
        onOpenChange={setPasteOpen}
        onPasteSuccess={handlePasteSuccess}
      />
    </section>
  );
}
