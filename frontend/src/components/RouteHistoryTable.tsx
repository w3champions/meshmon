import { formatDistanceToNowStrict } from "date-fns";
import { useState } from "react";
import type { components } from "@/api/schema.gen";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { cn } from "@/lib/utils";

type RouteSnapshotSummary = components["schemas"]["RouteSnapshotSummary"];

interface RouteHistoryTableProps {
  snapshots: RouteSnapshotSummary[];
  /**
   * `true` when the server clamped the list at its cap (100). Rendered as a
   * footnote under the table so operators don't silently miss older entries.
   */
  truncated?: boolean;
  onCompare: (pair: { a: number; b: number }) => void;
  className?: string;
}

/**
 * Path detail "Route change history" table. Rows come from the overview
 * endpoint's `recent_snapshots`, which is protocol-scoped server-side, so
 * all snapshots in one render share a protocol — no cross-protocol pair
 * is ever representable.
 */
export function RouteHistoryTable({
  snapshots,
  truncated,
  onCompare,
  className,
}: RouteHistoryTableProps) {
  const [a, setA] = useState<number | null>(null);
  const [b, setB] = useState<number | null>(null);

  if (snapshots.length === 0) {
    return <p className="text-sm text-muted-foreground">No route snapshots in this window.</p>;
  }

  return (
    <div className={cn("flex flex-col gap-3", className)}>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Observed</TableHead>
            <TableHead>Hops</TableHead>
            <TableHead>Avg RTT</TableHead>
            <TableHead>Loss</TableHead>
            <TableHead>A</TableHead>
            <TableHead>B</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {snapshots.map((s) => (
            <TableRow key={s.id}>
              <TableCell title={s.observed_at}>
                {formatDistanceToNowStrict(new Date(s.observed_at), { addSuffix: true })}
              </TableCell>
              <TableCell>{s.path_summary?.hop_count ?? "—"} hops</TableCell>
              <TableCell>
                {s.path_summary ? `${(s.path_summary.avg_rtt_micros / 1000).toFixed(0)} ms` : "—"}
              </TableCell>
              <TableCell>
                {s.path_summary ? `${(s.path_summary.loss_pct * 100).toFixed(1)}%` : "—"}
              </TableCell>
              <TableCell>
                <input
                  type="radio"
                  name="compare-a"
                  aria-label={`Pick as A (id ${s.id})`}
                  checked={a === s.id}
                  onChange={() => setA(s.id)}
                />
              </TableCell>
              <TableCell>
                <input
                  type="radio"
                  name="compare-b"
                  aria-label={`Pick as B (id ${s.id})`}
                  checked={b === s.id}
                  onChange={() => setB(s.id)}
                />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {truncated && (
        <p className="text-xs text-muted-foreground">
          Showing latest 100 snapshots — narrow the time window to see older entries.
        </p>
      )}
      <div className="flex gap-2">
        <Button
          type="button"
          disabled={!a || !b || a === b}
          onClick={() => a && b && onCompare({ a, b })}
        >
          Compare
        </Button>
        <Button
          type="button"
          variant="ghost"
          disabled={a === null && b === null}
          onClick={() => {
            setA(null);
            setB(null);
          }}
        >
          Clear
        </Button>
      </div>
    </div>
  );
}
