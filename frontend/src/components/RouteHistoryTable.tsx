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
  source: string;
  target: string;
  snapshots: RouteSnapshotSummary[];
  /**
   * `true` when the server clamped the list at its cap (100). Rendered as a
   * footnote under the table so operators don't silently miss older entries.
   */
  truncated?: boolean;
  onCompare: (pair: { a: number; b: number }) => void;
  className?: string;
}

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

  // Cross-protocol diffs are nonsense (ICMP vs TCP hop semantics differ), so
  // lock B to the same protocol as A (and vice versa). Native radios can't
  // be unchecked by clicking again, so the "Clear" button below is the
  // recovery path when a user wants to switch protocols.
  const pickedProtocol = (id: number | null): string | null =>
    id === null ? null : (snapshots.find((s) => s.id === id)?.protocol ?? null);
  const aProtocol = pickedProtocol(a);
  const bProtocol = pickedProtocol(b);

  return (
    <div className={cn("flex flex-col gap-3", className)}>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Observed</TableHead>
            <TableHead>Protocol</TableHead>
            <TableHead>Hops</TableHead>
            <TableHead>Avg RTT</TableHead>
            <TableHead>Loss</TableHead>
            <TableHead>A</TableHead>
            <TableHead>B</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {snapshots.map((s) => {
            // Disable the opposite-side radio when its row's protocol
            // doesn't match the already-picked side. Keeps comparisons
            // within a single protocol without surfacing a banner.
            const aDisabled = bProtocol !== null && s.protocol !== bProtocol;
            const bDisabled = aProtocol !== null && s.protocol !== aProtocol;
            return (
              <TableRow key={s.id}>
                <TableCell title={s.observed_at}>
                  {formatDistanceToNowStrict(new Date(s.observed_at), { addSuffix: true })}
                </TableCell>
                <TableCell className="uppercase text-xs">{s.protocol}</TableCell>
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
                    disabled={aDisabled}
                    onChange={() => setA(s.id)}
                  />
                </TableCell>
                <TableCell>
                  <input
                    type="radio"
                    name="compare-b"
                    aria-label={`Pick as B (id ${s.id})`}
                    checked={b === s.id}
                    disabled={bDisabled}
                    onChange={() => setB(s.id)}
                  />
                </TableCell>
              </TableRow>
            );
          })}
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
