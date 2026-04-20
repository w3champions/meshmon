import {
  type Column,
  type ColumnDef,
  flexRender,
  getCoreRowModel,
  useReactTable,
  type VisibilityState,
} from "@tanstack/react-table";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ChevronDown, ChevronsUpDown, ChevronUp, RefreshCw, Settings2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { CatalogueEntry, CatalogueSortBy, CatalogueSortDir } from "@/api/hooks/catalogue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Table, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { lookupCountryName } from "@/lib/countries";
import { cn } from "@/lib/utils";
import { StatusChip } from "./StatusChip";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Extracts the hostname from a raw website string entered by the operator.
 * Prepends `https://` when no scheme is present so `URL()` can parse it.
 * Falls back to returning the raw value unchanged on parse failure.
 */
export function formatWebsiteHost(raw: string): string {
  try {
    const normalized = /^https?:\/\//i.test(raw) ? raw : `https://${raw}`;
    return new URL(normalized).hostname;
  } catch {
    return raw;
  }
}

// ---------------------------------------------------------------------------
// Sort types
// ---------------------------------------------------------------------------

export type CatalogueSortCol = CatalogueSortBy;

/**
 * Table sort state. `col`/`dir` are both `null` only together (unsorted —
 * the server falls back to `created_at DESC` tiebroken on `id DESC`).
 */
export interface CatalogueTableSort {
  col: CatalogueSortCol | null;
  dir: CatalogueSortDir | null;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

export const LS_KEY = "catalogue.table.visibleColumns";

/** Columns that are visible by default. */
const DEFAULT_VISIBLE: string[] = [
  "ip",
  "display_name",
  "city",
  "country",
  "asn",
  "network",
  "status",
  "actions",
];

/** All optional (hideable) columns — off by default. */
const OPTIONAL_COLUMNS: string[] = ["location", "website", "notes"];

/**
 * Map from UI column id to backend `SortBy`. Only columns whose id differs
 * from the backend column name appear here; sortable columns whose id
 * already matches the backend variant are handled by the identity fallback
 * in {@link columnToSortBy}. Columns absent from this surface entirely
 * (e.g. `notes`, `actions`) yield `null` and render as plain headers.
 */
const COLUMN_TO_SORT_BY: Record<string, CatalogueSortCol> = {
  ip: "ip",
  display_name: "display_name",
  city: "city",
  country: "country_code",
  asn: "asn",
  network: "network_operator",
  status: "enrichment_status",
  website: "website",
  location: "location",
};

function columnToSortBy(columnId: string): CatalogueSortCol | null {
  return COLUMN_TO_SORT_BY[columnId] ?? null;
}

/** Estimated row height in px — matches the shadcn `TableRow` default. */
export const ROW_HEIGHT_ESTIMATE = 44;

/** Max height of the virtualized scroll container. */
const SCROLL_MAX_HEIGHT = "70vh";

/**
 * Explicit per-column widths (pixels). The header is a real `<table>`
 * with `table-layout: fixed` + `<colgroup>` so column titles align and
 * screen readers still read column headers. The virtualized body is a
 * flat stack of `role="row"` / `role="cell"` divs laid out with CSS
 * grid using these same widths — grid tracks behave predictably with
 * `minWidth: 0` for in-track truncation, whereas `<tr>`-as-flex with
 * `<td>` children yields undefined table layout across browsers.
 * Every cell renderer clips overflow via `truncate`, so widths
 * narrower than the raw content are safe.
 */
const COLUMN_WIDTHS: Record<string, number> = {
  ip: 140,
  display_name: 180,
  city: 140,
  country: 150,
  asn: 100,
  network: 180,
  status: 120,
  location: 110,
  website: 180,
  notes: 200,
  actions: 80,
};

/** Fallback width for any column that doesn't appear in {@link COLUMN_WIDTHS}. */
const DEFAULT_COLUMN_WIDTH = 140;

function columnWidth(columnId: string): number {
  return COLUMN_WIDTHS[columnId] ?? DEFAULT_COLUMN_WIDTH;
}

/**
 * CSS grid-template-columns value for the virtualized body grid.
 * Emits one fixed-px track per visible column in the configured order
 * so row cells line up with the header `<colgroup>` column widths.
 */
function buildGridTemplate(columns: ReadonlyArray<Column<CatalogueEntry, unknown>>): string {
  return columns.map((col) => `${columnWidth(col.id)}px`).join(" ");
}

/**
 * Initial viewport rect fed to the virtualizer before the ResizeObserver
 * fires its first callback. jsdom never measures layout, so a zero rect
 * would leave the virtualizer with nothing to render under tests. Picking
 * a generous window here keeps production correct (the observer
 * supersedes this value within a frame) while giving jsdom enough
 * virtual items to commit rows on the first render pass.
 */
const INITIAL_SCROLL_RECT = { width: 1024, height: 800 };

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface CatalogueTableProps {
  /** Flattened rows across every page of the infinite query. */
  rows: CatalogueEntry[];
  /** Server-reported total for the active filter (pre-page). */
  total: number;
  /** True while the next-page fetch can be triggered. */
  hasNextPage: boolean;
  /** True while a next-page fetch is in flight. */
  isFetchingNextPage: boolean;
  /** Trigger the next-page fetch. */
  fetchNextPage: () => void;
  /** Active sort state (parent-owned — URL round-trip happens upstream). */
  sort: CatalogueTableSort;
  /**
   * Called when the operator cycles a sort header. Fires with
   * `(col, dir)` for `asc`/`desc`, or `(null, null)` when the cycle
   * returns to unsorted.
   */
  onSortChange: (col: CatalogueSortCol | null, dir: CatalogueSortDir | null) => void;
  onRowClick: (id: string) => void;
  onReenrich: (id: string) => void;
  className?: string;
}

// ---------------------------------------------------------------------------
// localStorage helpers
// ---------------------------------------------------------------------------

/**
 * Load stored column visibility as a `Record<string, boolean>` map.
 *
 * Returns `null` when nothing is stored, the JSON is malformed, or the value
 * is the legacy array shape (list of visible IDs). The legacy shape cannot
 * distinguish "column is new" from "user hid it", so we deliberately ignore
 * it — new writes will replace it with the map shape.
 */
function loadStoredVisibility(): Record<string, boolean> | null {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
      return null;
    }
    const result: Record<string, boolean> = {};
    for (const [k, v] of Object.entries(parsed)) {
      if (typeof v === "boolean") result[k] = v;
    }
    return result;
  } catch {
    return null;
  }
}

function saveVisibility(state: VisibilityState): void {
  // Persist the full map so later reads can distinguish "column missing from
  // saved state" (falls back to compile-time default on hydration) from
  // "column explicitly hidden" (stored as `false`).
  localStorage.setItem(LS_KEY, JSON.stringify(state));
}

function getInitialVisibility(): VisibilityState {
  const stored = loadStoredVisibility();
  const result: VisibilityState = {};

  for (const col of DEFAULT_VISIBLE) {
    result[col] = stored && col in stored ? stored[col] : true;
  }
  for (const col of OPTIONAL_COLUMNS) {
    result[col] = stored && col in stored ? stored[col] : false;
  }

  return result;
}

// ---------------------------------------------------------------------------
// SortIcon / SortableHeader
// ---------------------------------------------------------------------------

interface SortIconProps {
  active: CatalogueSortDir | null;
}

function SortIcon({ active }: SortIconProps) {
  if (active === "asc") return <ChevronUp className="h-3.5 w-3.5" aria-hidden="true" />;
  if (active === "desc") return <ChevronDown className="h-3.5 w-3.5" aria-hidden="true" />;
  return <ChevronsUpDown className="h-3.5 w-3.5 opacity-50" aria-hidden="true" />;
}

interface SortableHeaderProps {
  col: CatalogueSortCol;
  label: string;
  sort: CatalogueTableSort;
  onSortChange: CatalogueTableProps["onSortChange"];
}

/**
 * Returns the next sort tuple in the three-state cycle
 * `none → asc → desc → none` for the given column.
 */
function nextSort(
  col: CatalogueSortCol,
  active: CatalogueSortDir | null,
): [CatalogueSortCol | null, CatalogueSortDir | null] {
  if (active === null) return [col, "asc"];
  if (active === "asc") return [col, "desc"];
  return [null, null];
}

/**
 * Interactive header contents. `aria-sort` lives on the enclosing `<th>`
 * (see call site) so assistive tech tracks the active column; this button
 * only owns the click + focus ring. Negative left margin lines the
 * chevron up with plain-text column titles.
 */
function SortableHeader({ col, label, sort, onSortChange }: SortableHeaderProps) {
  const active = sort.col === col ? sort.dir : null;
  const [nextCol, nextDir] = nextSort(col, active);

  return (
    <button
      type="button"
      onClick={() => onSortChange(nextCol, nextDir)}
      className="-ml-1 flex items-center gap-1 rounded-sm px-1 hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      {label}
      <SortIcon active={active} />
    </button>
  );
}

/**
 * `aria-sort` value for a header cell given the active column and direction.
 * `ascending` / `descending` only on the currently-sorted column; every
 * other sortable header reports `none`.
 */
function ariaSortForColumn(
  col: CatalogueSortCol,
  sort: CatalogueTableSort,
): "ascending" | "descending" | "none" {
  if (sort.col !== col) return "none";
  if (sort.dir === "asc") return "ascending";
  if (sort.dir === "desc") return "descending";
  return "none";
}

// ---------------------------------------------------------------------------
// Column definitions
// ---------------------------------------------------------------------------

/**
 * All columns except Actions. Actions is kept separate so it can always be
 * appended last — even when optional columns are toggled on.
 */
function buildNonActionColumns(): ColumnDef<CatalogueEntry>[] {
  return [
    {
      id: "ip",
      accessorKey: "ip",
      header: "IP",
      cell: ({ row }) => (
        <span className="block truncate font-mono text-xs" title={row.original.ip}>
          {row.original.ip}
        </span>
      ),
    },
    {
      id: "display_name",
      accessorKey: "display_name",
      header: "Name",
      cell: ({ row }) => {
        const name = row.original.display_name;
        return (
          <span className="block truncate" title={name ?? undefined}>
            {name ?? "—"}
          </span>
        );
      },
    },
    {
      id: "city",
      accessorKey: "city",
      header: "City",
      cell: ({ row }) => {
        const city = row.original.city;
        return (
          <span className="block truncate" title={city ?? undefined}>
            {city ?? "—"}
          </span>
        );
      },
    },
    {
      id: "country",
      accessorKey: "country_code",
      header: "Country",
      cell: ({ row }) => {
        const code = row.original.country_code;
        const name = lookupCountryName(code);
        const display = name ?? code ?? "—";
        return (
          <span className="block truncate" title={code ?? undefined}>
            {display}
          </span>
        );
      },
    },
    {
      id: "asn",
      accessorKey: "asn",
      header: "ASN",
      cell: ({ row }) => {
        const asn = row.original.asn != null ? String(row.original.asn) : "—";
        return (
          <span className="block truncate" title={asn}>
            {asn}
          </span>
        );
      },
    },
    {
      id: "network",
      accessorKey: "network_operator",
      header: "Network",
      cell: ({ row }) => {
        const net = row.original.network_operator;
        return (
          <span className="block truncate" title={net ?? undefined}>
            {net ?? "—"}
          </span>
        );
      },
    },
    {
      id: "status",
      header: "Status",
      cell: ({ row }) => {
        // Display-only in the table: the Actions column owns the re-enrich button.
        // StatusChip.onReenrich is still used in EntryDrawer where the chip is
        // the only re-enrich surface.
        return <StatusChip status={row.original.enrichment_status} />;
      },
    },
    // Optional columns — off by default
    {
      id: "location",
      header: "Location",
      cell: ({ row }) => {
        const hasCoords = row.original.latitude != null && row.original.longitude != null;
        return (
          <Badge variant={hasCoords ? "secondary" : "outline"}>
            {hasCoords ? "Present" : "Unset"}
          </Badge>
        );
      },
    },
    {
      id: "website",
      accessorKey: "website",
      header: "Website",
      cell: ({ row }) => {
        const website = row.original.website;
        if (!website) return <span className="block truncate">—</span>;
        // Operators may save "example.com" as well as a full URL; normalise
        // so the href is always absolute. Assume https when no scheme is set.
        const href = /^https?:\/\//i.test(website) ? website : `https://${website}`;
        const displayHost = formatWebsiteHost(website);
        return (
          <a
            href={href}
            target="_blank"
            rel="noopener noreferrer"
            onClick={(e) => e.stopPropagation()}
            className="block truncate text-primary underline-offset-2 hover:underline"
            title={website}
          >
            {displayHost}
          </a>
        );
      },
    },
    {
      id: "notes",
      accessorKey: "notes",
      header: "Notes",
      cell: ({ row }) => (
        <span className="block truncate" title={row.original.notes ?? undefined}>
          {row.original.notes ?? "—"}
        </span>
      ),
    },
  ];
}

/** The Actions column — always rendered last regardless of optional column state. */
function buildActionsColumn(onReenrich: (id: string) => void): ColumnDef<CatalogueEntry> {
  return {
    id: "actions",
    header: "Actions",
    cell: ({ row }) => {
      const { id, ip } = row.original;
      return (
        <Button
          type="button"
          variant="ghost"
          size="icon"
          aria-label={`Re-enrich ${ip}`}
          onClick={(e) => {
            e.stopPropagation();
            onReenrich(id);
          }}
        >
          <RefreshCw className="h-4 w-4" aria-hidden="true" />
        </Button>
      );
    },
  };
}

/**
 * Assembles the full column list with Actions pinned to the rightmost position,
 * regardless of which optional columns are visible.
 */
function buildColumns(onReenrich: (id: string) => void): ColumnDef<CatalogueEntry>[] {
  return [...buildNonActionColumns(), buildActionsColumn(onReenrich)];
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function CatalogueTable({
  rows,
  total,
  hasNextPage,
  isFetchingNextPage,
  fetchNextPage,
  sort,
  onSortChange,
  onRowClick,
  onReenrich,
  className,
}: CatalogueTableProps) {
  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>(getInitialVisibility);

  // Persist visibility changes to localStorage
  useEffect(() => {
    saveVisibility(columnVisibility);
  }, [columnVisibility]);

  const columns = useMemo(() => buildColumns(onReenrich), [onReenrich]);

  const table = useReactTable({
    data: rows,
    columns,
    state: { columnVisibility },
    onColumnVisibilityChange: setColumnVisibility,
    getCoreRowModel: getCoreRowModel(),
  });

  // Visible columns in the configured order. react-table owns column
  // definitions + visibility here; we drive row rendering ourselves via the
  // virtualizer so the existing column-chooser plumbing is preserved.
  const visibleColumns = table.getVisibleLeafColumns();
  // All toggleable columns (default visible + optional) — feeds the chooser.
  const toggleableColumns = table.getAllLeafColumns();
  // react-table's core row model, indexable by the virtualizer's `index`.
  const tableRows = table.getRowModel().rows;

  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: tableRows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT_ESTIMATE,
    overscan: 8,
    // jsdom-friendly fallback: the ResizeObserver replaces this with the
    // real element rect once layout runs in a browser. See constant
    // docstring for rationale.
    initialRect: INITIAL_SCROLL_RECT,
  });

  const virtualItems = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();

  return (
    <div className={cn("flex flex-col gap-3", className)}>
      {/* Column chooser */}
      <div className="flex justify-end">
        <Popover>
          <PopoverTrigger asChild>
            <Button variant="outline" size="sm" aria-label="Columns">
              <Settings2 className="mr-2 h-4 w-4" aria-hidden="true" />
              Columns
            </Button>
          </PopoverTrigger>
          <PopoverContent align="end" className="w-48 p-2">
            <fieldset className="flex flex-col gap-1">
              <legend className="mb-1 text-xs font-semibold text-muted-foreground">
                Visible columns
              </legend>
              {toggleableColumns.map((col) => {
                const header = col.columnDef.header;
                const label = typeof header === "string" ? header : col.id;
                return (
                  <label key={col.id} className="flex cursor-pointer items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={col.getIsVisible()}
                      onChange={col.getToggleVisibilityHandler()}
                      aria-label={label}
                    />
                    {label}
                  </label>
                );
              })}
            </fieldset>
          </PopoverContent>
        </Popover>
      </div>

      {/* Table — the header stays a real `<table>` so screen readers
          announce column headers and aria-sort. The virtualized body
          below is intentionally NOT a `<table>`: rendering `<tr
          style="display: flex">` with `<td>` children inside a
          `table-layout: fixed` `<table>` yields undefined layout
          across browsers (Chrome clips all but the first cell). We
          render rows as div-based CSS-grid tracks that use the same
          per-column widths as the header `<colgroup>`, so the two
          still line up visually. */}
      <div className="overflow-hidden rounded-md border">
        <Table style={{ tableLayout: "fixed" }}>
          <colgroup>
            {visibleColumns.map((col) => (
              <col key={col.id} style={{ width: `${columnWidth(col.id)}px` }} />
            ))}
          </colgroup>
          <TableHeader>
            <TableRow>
              {visibleColumns.map((col) => {
                const sortCol = columnToSortBy(col.id);
                const header = col.columnDef.header;
                const label = typeof header === "string" ? header : col.id;
                return (
                  <TableHead
                    key={col.id}
                    aria-sort={sortCol ? ariaSortForColumn(sortCol, sort) : undefined}
                    className="overflow-hidden"
                  >
                    {sortCol ? (
                      <SortableHeader
                        col={sortCol}
                        label={label}
                        sort={sort}
                        onSortChange={onSortChange}
                      />
                    ) : (
                      label
                    )}
                  </TableHead>
                );
              })}
            </TableRow>
          </TableHeader>
        </Table>
        <div
          ref={scrollRef}
          className="relative overflow-auto"
          style={{ maxHeight: SCROLL_MAX_HEIGHT }}
        >
          <div style={{ position: "relative", height: `${totalSize}px` }}>
            {virtualItems.map((virtualItem) => {
              const row = tableRows[virtualItem.index];
              if (!row) return null;
              const handleClick = () => onRowClick(row.original.id);
              return (
                /* biome-ignore lint/a11y/useSemanticElements: virtualized row is a CSS-grid parent for cell tracks (see block comment above); a <button> would not accept grid children semantically and nesting the grid inside a <button> would break the ARIA row/cell structure. role="button" keeps keyboard+click affordance. */
                <div
                  key={row.original.id}
                  data-index={virtualItem.index}
                  role="button"
                  tabIndex={0}
                  aria-label={`Open entry ${row.original.ip}`}
                  onClick={handleClick}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      handleClick();
                    }
                  }}
                  className="absolute top-0 left-0 grid w-full cursor-pointer items-center overflow-hidden border-b text-sm hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none"
                  style={{
                    transform: `translateY(${virtualItem.start}px)`,
                    height: `${virtualItem.size}px`,
                    gridTemplateColumns: buildGridTemplate(visibleColumns),
                  }}
                >
                  {row.getVisibleCells().map((cell) => (
                    /* biome-ignore lint/a11y/useSemanticElements: virtualized body is pure-div CSS grid (see block comment above); a <td> here would require a <tr>/<table> ancestor and reintroduce the Chrome flex-tr layout bug. role="cell" keeps ARIA table semantics for screen readers. */
                    <div
                      key={cell.id}
                      role="cell"
                      className="overflow-hidden px-4"
                      style={{ minWidth: 0 }}
                    >
                      {flexRender(cell.column.columnDef.cell, cell.getContext())}
                    </div>
                  ))}
                </div>
              );
            })}
          </div>
        </div>
        {/* Load-more + total counter */}
        <div className="flex items-center justify-between border-t bg-muted/20 px-4 py-3 text-sm">
          <span className="text-muted-foreground">
            {rows.length} of {total} entries
          </span>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={!hasNextPage || isFetchingNextPage}
            onClick={() => fetchNextPage()}
          >
            {isFetchingNextPage ? "Loading…" : hasNextPage ? "Load more" : "All loaded"}
          </Button>
        </div>
      </div>
    </div>
  );
}
