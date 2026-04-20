import { useVirtualizer } from "@tanstack/react-virtual";
import { useMemo, useRef, useState } from "react";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { useCatalogueListInfinite } from "@/api/hooks/catalogue";
import type { components } from "@/api/schema.gen";
import { PasteStaging } from "@/components/catalogue/PasteStaging";
import { FilterRail, type FilterValue } from "@/components/filter/FilterRail";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { destinationFilterToQuery } from "@/lib/catalogue-query";
import { lookupCountryName } from "@/lib/countries";
import { cn } from "@/lib/utils";

type FacetsResponse = components["schemas"]["FacetsResponse"];

const ROW_HEIGHT = 44;
const SCROLL_MAX_HEIGHT = "60vh";
const INITIAL_SCROLL_RECT = { width: 1024, height: 600 };
const PAGE_SIZE = 100;

/**
 * Shared CSS grid track expression for the header + virtualized body.
 * Columns: IP, Name, City, Country, ASN, Network, selected-check.
 */
const GRID_TEMPLATE =
  "120px minmax(140px, 1.2fr) minmax(100px, 1fr) minmax(110px, 1fr) 80px minmax(140px, 1.5fr) 32px";

export interface DestinationPanelProps {
  selected: Set<string>;
  onSelectedChange(next: Set<string>): void;
  filter: FilterValue;
  onFilterChange(next: FilterValue): void;
  facets: FacetsResponse | undefined;
  onOpenMap(): void;
  /**
   * When true, every mutation button (Add all / Add matching / Remove all /
   * Add IPs / Map view) is disabled and row-click no-ops. Filter rail stays
   * live so the operator can still inspect matches. Composer sets this
   * after a draft campaign has been created so further destination edits
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

  const rows: CatalogueEntry[] = useMemo(
    () => listQuery.data?.pages.flatMap((p) => p.entries) ?? [],
    [listQuery.data],
  );

  const total = listQuery.data?.pages[0]?.total ?? 0;
  const shapesActive = filter.shapes.length > 0;

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

  const handleAddVisible = () => {
    if (disabled) return;
    // F1 scope: snapshot IPs from already-loaded pages only.
    // The catalogue-walk fallback that covers the "total > loaded rows" case
    // lives in F2's CampaignComposer page (plan T47 F.7 §608).
    addIps(rows.map((r) => r.ip));
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
            {/*
              Plan F.3 §527: "Add all" and "Add matching" share the same handler
              in F1 — both snapshot the currently loaded rows. Split if F2
              changes semantics (e.g. when the catalogue-walk fallback at F.7
              §608 lands).
            */}
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={handleAddVisible}
              disabled={disabled || rows.length === 0}
            >
              Add all
            </Button>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={handleAddVisible}
              disabled={disabled || rows.length === 0}
            >
              Add matching
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
                    <div role="cell" className="truncate px-3 font-mono text-xs">
                      {entry.ip}
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
