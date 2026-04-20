import {
  type ColumnDef,
  flexRender,
  getCoreRowModel,
  useReactTable,
  type VisibilityState,
} from "@tanstack/react-table";
import { RefreshCw, Settings2 } from "lucide-react";
import { useEffect, useState } from "react";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { cn } from "@/lib/utils";
import { StatusChip } from "./StatusChip";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const LS_KEY = "catalogue.table.visibleColumns";

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
const OPTIONAL_COLUMNS: string[] = ["latitude", "longitude", "website", "notes"];

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface CatalogueTableProps {
  entries: CatalogueEntry[];
  onRowClick: (id: string) => void;
  onReenrich: (id: string) => void;
  className?: string;
}

// ---------------------------------------------------------------------------
// localStorage helpers
// ---------------------------------------------------------------------------

function loadVisibleIds(): string[] | null {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    return parsed as string[];
  } catch {
    return null;
  }
}

function saveVisibility(state: VisibilityState): void {
  const visibleIds = Object.entries(state)
    .filter(([, visible]) => visible)
    .map(([id]) => id);
  localStorage.setItem(LS_KEY, JSON.stringify(visibleIds));
}

function getInitialVisibility(): VisibilityState {
  const stored = loadVisibleIds();
  const allColumns = [...DEFAULT_VISIBLE, ...OPTIONAL_COLUMNS];
  const result: VisibilityState = {};

  if (stored) {
    for (const col of allColumns) {
      result[col] = stored.includes(col);
    }
  } else {
    for (const col of DEFAULT_VISIBLE) result[col] = true;
    for (const col of OPTIONAL_COLUMNS) result[col] = false;
  }

  return result;
}

// ---------------------------------------------------------------------------
// Column definitions
// ---------------------------------------------------------------------------

function buildColumns(onReenrich: (id: string) => void): ColumnDef<CatalogueEntry>[] {
  return [
    {
      id: "ip",
      accessorKey: "ip",
      header: "IP",
      cell: ({ row }) => <span className="font-mono text-xs">{row.original.ip}</span>,
    },
    {
      id: "display_name",
      accessorKey: "display_name",
      header: "Name",
      cell: ({ row }) => row.original.display_name ?? "—",
    },
    {
      id: "city",
      accessorKey: "city",
      header: "City",
      cell: ({ row }) => row.original.city ?? "—",
    },
    {
      id: "country",
      header: "Country",
      cell: ({ row }) => {
        const { country_code, country_name } = row.original;
        if (!country_code) return "—";
        return <span title={country_name ?? undefined}>{country_code}</span>;
      },
    },
    {
      id: "asn",
      accessorKey: "asn",
      header: "ASN",
      cell: ({ row }) => (row.original.asn != null ? String(row.original.asn) : "—"),
    },
    {
      id: "network",
      accessorKey: "network_operator",
      header: "Network",
      cell: ({ row }) => row.original.network_operator ?? "—",
    },
    {
      id: "status",
      header: "Status",
      cell: ({ row }) => {
        const { enrichment_status, operator_edited_fields } = row.original;
        // Display-only in the table: the Actions column owns the re-enrich button.
        // StatusChip.onReenrich is still used in EntryDrawer where the chip is
        // the only re-enrich surface.
        return (
          <StatusChip
            status={enrichment_status}
            operatorLocked={operator_edited_fields.length > 0}
          />
        );
      },
    },
    {
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
    },
    // Optional columns — off by default
    {
      id: "latitude",
      accessorKey: "latitude",
      header: "Latitude",
      cell: ({ row }) => (row.original.latitude != null ? String(row.original.latitude) : "—"),
    },
    {
      id: "longitude",
      accessorKey: "longitude",
      header: "Longitude",
      cell: ({ row }) => (row.original.longitude != null ? String(row.original.longitude) : "—"),
    },
    {
      id: "website",
      accessorKey: "website",
      header: "Website",
      cell: ({ row }) => row.original.website ?? "—",
    },
    {
      id: "notes",
      accessorKey: "notes",
      header: "Notes",
      cell: ({ row }) => row.original.notes ?? "—",
    },
  ];
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function CatalogueTable({
  entries,
  onRowClick,
  onReenrich,
  className,
}: CatalogueTableProps) {
  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>(getInitialVisibility);

  // Persist visibility changes to localStorage
  useEffect(() => {
    saveVisibility(columnVisibility);
  }, [columnVisibility]);

  const columns = buildColumns(onReenrich);

  const table = useReactTable({
    data: entries,
    columns,
    state: { columnVisibility },
    onColumnVisibilityChange: setColumnVisibility,
    getCoreRowModel: getCoreRowModel(),
  });

  // All toggleable columns (default visible + optional)
  const toggleableColumns = table.getAllLeafColumns();

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

      {/* Table */}
      <Table>
        <TableHeader>
          {table.getHeaderGroups().map((hg) => (
            <TableRow key={hg.id}>
              {hg.headers.map((h) => (
                <TableHead key={h.id}>
                  {flexRender(h.column.columnDef.header, h.getContext())}
                </TableHead>
              ))}
            </TableRow>
          ))}
        </TableHeader>
        <TableBody>
          {table.getRowModel().rows.map((row) => {
            const handleClick = () => onRowClick(row.original.id);
            return (
              <TableRow
                key={row.id}
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
                className="cursor-pointer hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none"
              >
                {row.getVisibleCells().map((cell) => (
                  <TableCell key={cell.id}>
                    {flexRender(cell.column.columnDef.cell, cell.getContext())}
                  </TableCell>
                ))}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </div>
  );
}
