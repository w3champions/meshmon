import type { components } from "@/api/schema.gen";
import { IpHostname } from "@/components/ip-hostname";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { cn } from "@/lib/utils";

type HopJson = components["schemas"]["HopJson"];

export interface RouteTableDiff {
  changedPositions: Set<number>;
  addedPositions: Set<number>;
  removedPositions: Set<number>;
}

type DiffState = "same" | "changed" | "added" | "removed";

interface RouteTableProps {
  hops: HopJson[];
  diff?: RouteTableDiff;
  className?: string;
}

function dominantIp(hop: HopJson): { ip: string; freq: number } {
  if (!hop.observed_ips || hop.observed_ips.length === 0) {
    return { ip: "—", freq: 0 };
  }
  let best = hop.observed_ips[0];
  for (const candidate of hop.observed_ips) {
    if (candidate.freq > best.freq) best = candidate;
  }
  return best;
}

function fmtMs(us: number): string {
  const ms = us / 1000;
  return `${ms.toFixed(us < 10_000 ? 1 : 0)} ms`;
}

function fmtPct(fraction: number): string {
  return `${(fraction * 100).toFixed(2)}%`;
}

function diffState(position: number, diff: RouteTableDiff | undefined): DiffState {
  if (!diff) return "same";
  if (diff.changedPositions.has(position)) return "changed";
  if (diff.addedPositions.has(position)) return "added";
  if (diff.removedPositions.has(position)) return "removed";
  return "same";
}

const ROW_TINT: Record<DiffState, string> = {
  changed: "bg-amber-500/10",
  added: "bg-emerald-500/10",
  removed: "bg-red-500/10",
  same: "",
};

const STATUS_LABEL: Record<DiffState, string | null> = {
  changed: "★ changed",
  added: "+ added",
  removed: "− removed",
  same: null,
};

export function RouteTable({ hops, diff, className }: RouteTableProps) {
  if (hops.length === 0) {
    return (
      <p className={cn("text-sm text-muted-foreground", className)}>
        No hops recorded in this snapshot.
      </p>
    );
  }

  return (
    <Table className={className}>
      <TableHeader>
        <TableRow>
          <TableHead className="w-12">TTL</TableHead>
          <TableHead>Hostname</TableHead>
          <TableHead className="w-20">Freq.</TableHead>
          <TableHead className="w-28">Avg RTT</TableHead>
          <TableHead className="w-24">Loss</TableHead>
          <TableHead className="w-32">Status</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {hops.map((h) => {
          const { ip, freq } = dominantIp(h);
          const state = diffState(h.position, diff);
          const status = STATUS_LABEL[state];
          return (
            <TableRow key={h.position} data-diff-state={state} className={ROW_TINT[state]}>
              <TableCell>{h.position}</TableCell>
              <TableCell>
                <IpHostname ip={ip} />
              </TableCell>
              <TableCell>{freq < 1 ? `${Math.round(freq * 100)}%` : "—"}</TableCell>
              <TableCell>{fmtMs(h.avg_rtt_micros)}</TableCell>
              <TableCell>{fmtPct(h.loss_pct)}</TableCell>
              <TableCell className="text-xs">{status && <span>{status}</span>}</TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
