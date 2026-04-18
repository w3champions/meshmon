import { useNavigate } from "@tanstack/react-router";
import {
  type ColumnDef,
  flexRender,
  getCoreRowModel,
  getFilteredRowModel,
  getSortedRowModel,
  type SortingState,
  useReactTable,
} from "@tanstack/react-table";
import { formatDistanceToNowStrict } from "date-fns";
import { useState } from "react";
import type { AgentSummary } from "@/api/hooks/agents";
import { StatusBadge } from "@/components/StatusBadge";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { isStale } from "@/lib/health";
import { cn } from "@/lib/utils";

interface AgentsTableProps {
  agents: AgentSummary[];
  className?: string;
}

const columns: ColumnDef<AgentSummary>[] = [
  { accessorKey: "id", header: "ID" },
  { accessorKey: "display_name", header: "Name" },
  { accessorKey: "location", header: "Location" },
  { accessorKey: "ip", header: "IP" },
  { accessorKey: "agent_version", header: "Version" },
  {
    accessorKey: "last_seen_at",
    header: "Last seen",
    cell: ({ getValue }) =>
      formatDistanceToNowStrict(new Date(getValue<string>()), { addSuffix: true }),
  },
  {
    id: "status",
    header: "Status",
    cell: ({ row }) => (
      <StatusBadge state={isStale(row.original.last_seen_at) ? "stale" : "online"} />
    ),
  },
];

export function AgentsTable({ agents, className }: AgentsTableProps) {
  const [sorting, setSorting] = useState<SortingState>([]);
  const [globalFilter, setGlobalFilter] = useState("");
  const navigate = useNavigate();

  const table = useReactTable({
    data: agents,
    columns,
    state: { sorting, globalFilter },
    onSortingChange: setSorting,
    onGlobalFilterChange: setGlobalFilter,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
    globalFilterFn: (row, _columnId, value: string) => {
      const v = value.toLowerCase();
      return (
        row.original.id.toLowerCase().includes(v) ||
        row.original.display_name.toLowerCase().includes(v)
      );
    },
  });

  return (
    <div className={cn("flex flex-col gap-3", className)}>
      <Input
        placeholder="Filter agents…"
        value={globalFilter}
        onChange={(e) => setGlobalFilter(e.target.value)}
        className="max-w-sm"
        aria-label="Filter agents"
      />
      <Table>
        <TableHeader>
          {table.getHeaderGroups().map((hg) => (
            <TableRow key={hg.id}>
              {hg.headers.map((h) => (
                <TableHead
                  key={h.id}
                  onClick={h.column.getCanSort() ? h.column.getToggleSortingHandler() : undefined}
                  className={h.column.getCanSort() ? "cursor-pointer select-none" : undefined}
                  aria-sort={
                    h.column.getCanSort()
                      ? h.column.getIsSorted() === "asc"
                        ? "ascending"
                        : h.column.getIsSorted() === "desc"
                          ? "descending"
                          : "none"
                      : undefined
                  }
                >
                  {flexRender(h.column.columnDef.header, h.getContext())}
                  {h.column.getIsSorted() === "asc"
                    ? " ▲"
                    : h.column.getIsSorted() === "desc"
                      ? " ▼"
                      : ""}
                </TableHead>
              ))}
            </TableRow>
          ))}
        </TableHeader>
        <TableBody>
          {table.getRowModel().rows.map((row) => {
            const go = () => navigate({ to: "/agents/$id", params: { id: row.original.id } });
            return (
              <TableRow
                key={row.id}
                role="link"
                tabIndex={0}
                aria-label={`Open agent ${row.original.id}`}
                // If a cell gains an interactive child (e.g. a copy button), that child's
                // handler must call `e.stopPropagation()` so the row-level navigation
                // doesn't swallow the click.
                onClick={go}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    go();
                  }
                }}
                className="cursor-pointer hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none"
              >
                {row.getVisibleCells().map((cell, idx) => (
                  <TableCell key={cell.id} className={idx === 0 ? "font-mono text-xs" : ""}>
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
