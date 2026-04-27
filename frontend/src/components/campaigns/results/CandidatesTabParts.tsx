/**
 * Presentational sub-components for {@link CandidatesTab}. Kept in a
 * sibling file so the tab module itself stays under the 400-line ceiling.
 *
 * Per-pair operator actions (force re-measure, dispatch detail for a
 * single pair) live inside the drilldown dialog instead of on every
 * candidate row — the action requires a `(source_agent_id,
 * destination_ip)` tuple, which is reachable only from the paginated
 * `…/candidates/{ip}/pair_details` endpoint that the dialog already
 * fetches.
 */

import { useMemo } from "react";
import type { Evaluation } from "@/api/hooks/evaluation";
import { CandidateRef } from "@/components/campaigns/CandidateRef";
import { RouteMixBar } from "@/components/campaigns/RouteMixBar";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { cn } from "@/lib/utils";

type Candidate = Evaluation["results"]["candidates"][number];

// ---------------------------------------------------------------------------
// Unqualified-reasons card
// ---------------------------------------------------------------------------

export interface UnqualifiedReasonsProps {
  reasons: Record<string, string>;
}

export function UnqualifiedReasons({ reasons }: UnqualifiedReasonsProps) {
  const entries = Object.entries(reasons);
  if (entries.length === 0) return null;
  return (
    <Card className="flex flex-col gap-2 p-4">
      <h3 className="text-sm font-semibold">Unqualified candidates</h3>
      <ul className="flex flex-col gap-1 text-sm text-muted-foreground">
        {entries.map(([ip, reason]) => (
          <li key={ip}>
            <span className="font-mono text-xs">{ip}</span> — {reason}
          </li>
        ))}
      </ul>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// Edge KPI strip
// ---------------------------------------------------------------------------

export interface EdgeKPIStripProps {
  evaluation: Evaluation;
}

/**
 * KPI strip for `edge_candidate` evaluation mode.
 * Shows: candidatesTotal, total destinations evaluated (sum across candidates),
 * and useful_latency_ms threshold snapshot.
 */
export function EdgeKPIStrip({ evaluation }: EdgeKPIStripProps) {
  const totalDestinations = useMemo(() => {
    // Sum destinations_total across candidates as a rough total
    let sum = 0;
    for (const c of evaluation.results.candidates) {
      if (c.destinations_total != null) sum = Math.max(sum, c.destinations_total);
    }
    return sum || null;
  }, [evaluation.results.candidates]);

  return (
    <section aria-label="Edge evaluation summary" className="grid grid-cols-2 gap-3 sm:grid-cols-3">
      <EdgeKpiCard label="Candidates" value={evaluation.candidates_total.toLocaleString()} />
      <EdgeKpiCard
        label="Destinations"
        value={totalDestinations != null ? totalDestinations.toLocaleString() : "—"}
      />
      <EdgeKpiCard
        label="Latency threshold (T)"
        value={
          evaluation.useful_latency_ms != null
            ? `${evaluation.useful_latency_ms.toFixed(0)} ms`
            : "—"
        }
      />
    </section>
  );
}

interface EdgeKpiCardProps {
  label: string;
  value: string;
}

function EdgeKpiCard({ label, value }: EdgeKpiCardProps) {
  return (
    <Card className="flex flex-col gap-1 p-4">
      <span className="text-xs uppercase tracking-wide text-muted-foreground">{label}</span>
      <span className="text-2xl font-semibold tabular-nums">{value}</span>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// Edge candidate table sort
// ---------------------------------------------------------------------------

export type EdgeCandidateSortColumn =
  | "coverage_count"
  | "display_name"
  | "destination_ip"
  | "mean_ms_under_t"
  | "coverage_weighted_ping_ms";

export type EdgeCandidateSort = {
  col: EdgeCandidateSortColumn;
  dir: "asc" | "desc";
};

// ---------------------------------------------------------------------------
// Edge candidate table
// ---------------------------------------------------------------------------

export interface EdgeCandidateTableProps {
  evaluation: Evaluation;
  selectedIp: string | null;
  onSelectCandidate: (ip: string) => void;
  sort: EdgeCandidateSort;
  onSortChange: (next: EdgeCandidateSort) => void;
}

function formatMs(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  return `${value.toFixed(1)} ms`;
}

function compareEdge(a: Candidate, b: Candidate, col: EdgeCandidateSortColumn): number {
  switch (col) {
    case "coverage_count":
      return (a.coverage_count ?? -Infinity) - (b.coverage_count ?? -Infinity);
    case "display_name":
      return (a.display_name ?? a.destination_ip).localeCompare(b.display_name ?? b.destination_ip);
    case "destination_ip":
      return a.destination_ip.localeCompare(b.destination_ip);
    case "mean_ms_under_t":
      return (a.mean_ms_under_t ?? Infinity) - (b.mean_ms_under_t ?? Infinity);
    case "coverage_weighted_ping_ms":
      return (a.coverage_weighted_ping_ms ?? Infinity) - (b.coverage_weighted_ping_ms ?? Infinity);
  }
}

function sortEdgeCandidates(rows: Candidate[], sort: EdgeCandidateSort): Candidate[] {
  const multiplier = sort.dir === "asc" ? 1 : -1;
  const indexed = rows.map((row, idx) => ({ row, idx }));
  indexed.sort((a, b) => {
    const primary = compareEdge(a.row, b.row, sort.col);
    if (primary !== 0) return primary * multiplier;
    return a.idx - b.idx;
  });
  return indexed.map(({ row }) => row);
}

interface EdgeSortableHeadProps {
  column: EdgeCandidateSortColumn;
  label: string;
  sort: EdgeCandidateSort;
  onSortChange: (next: EdgeCandidateSort) => void;
  className?: string;
}

function EdgeSortableHead({ column, label, sort, onSortChange, className }: EdgeSortableHeadProps) {
  const active = sort.col === column;
  const nextDir: "asc" | "desc" = active && sort.dir === "desc" ? "asc" : "desc";
  const ariaSort = active ? (sort.dir === "asc" ? "ascending" : "descending") : "none";
  return (
    <TableHead className={className} aria-sort={ariaSort}>
      <button
        type="button"
        className="flex items-center gap-1 text-left font-medium hover:text-foreground"
        onClick={() => onSortChange({ col: column, dir: nextDir })}
      >
        {label}
        <span aria-hidden className="text-xs text-muted-foreground">
          {active ? (sort.dir === "asc" ? "▲" : "▼") : "↕"}
        </span>
      </button>
    </TableHead>
  );
}

export function EdgeCandidateTable({
  evaluation,
  selectedIp,
  onSelectCandidate,
  sort,
  onSortChange,
}: EdgeCandidateTableProps) {
  const rows = useMemo(
    () => sortEdgeCandidates(evaluation.results.candidates, sort),
    [evaluation.results.candidates, sort],
  );

  return (
    <div className="flex flex-col gap-3">
      <EdgeKPIStrip evaluation={evaluation} />

      {rows.length === 0 ? (
        <Card className="p-6 text-sm text-muted-foreground" role="status">
          No edge candidates matched the evaluator&apos;s scoring pass.
        </Card>
      ) : (
        <Card className="overflow-hidden">
          <Table aria-label="Edge evaluation candidates">
            <TableHeader>
              <TableRow>
                <EdgeSortableHead
                  column="coverage_count"
                  label="#"
                  sort={sort}
                  onSortChange={onSortChange}
                  className="w-10"
                />
                <TableHead>Candidate</TableHead>
                <EdgeSortableHead
                  column="coverage_count"
                  label="Coverage"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                <EdgeSortableHead
                  column="mean_ms_under_t"
                  label="Mean ping under T"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                <TableHead>Route mix</TableHead>
                <EdgeSortableHead
                  column="coverage_weighted_ping_ms"
                  label="Coverage-wtd ping"
                  sort={sort}
                  onSortChange={onSortChange}
                />
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((candidate, index) => {
                const isSelected = selectedIp === candidate.destination_ip;
                const refData = {
                  ip: candidate.destination_ip,
                  display_name: candidate.display_name,
                  city: candidate.city,
                  country_code: candidate.country_code,
                  asn: candidate.asn,
                  network_operator: candidate.network_operator,
                  hostname: candidate.hostname,
                  is_mesh_member: candidate.is_mesh_member,
                  agent_id: (candidate as Candidate & { agent_id?: string | null }).agent_id,
                };

                const direct = candidate.direct_share ?? 0;
                const oneHop = candidate.onehop_share ?? 0;
                const twoHop = candidate.twohop_share ?? 0;

                return (
                  <TableRow
                    key={candidate.destination_ip}
                    data-state={isSelected ? "selected" : undefined}
                    data-testid={`edge-candidate-row-${candidate.destination_ip}`}
                    className="cursor-pointer"
                    onClick={() => onSelectCandidate(candidate.destination_ip)}
                  >
                    <TableCell className="text-muted-foreground">{index + 1}</TableCell>
                    <TableCell>
                      <CandidateRef mode="compact" data={refData} />
                    </TableCell>
                    <TableCell>
                      <CoverageChip
                        count={candidate.coverage_count}
                        total={candidate.destinations_total}
                      />
                    </TableCell>
                    <TableCell className="text-sm tabular-nums">
                      {formatMs(candidate.mean_ms_under_t)}
                    </TableCell>
                    <TableCell>
                      <div className="w-24">
                        <RouteMixBar direct={direct} oneHop={oneHop} twoHop={twoHop} />
                      </div>
                    </TableCell>
                    <TableCell
                      className={cn(
                        "text-sm tabular-nums",
                        candidate.coverage_weighted_ping_ms != null ? "" : "text-muted-foreground",
                      )}
                    >
                      {formatMs(candidate.coverage_weighted_ping_ms)}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Card>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Coverage chip
// ---------------------------------------------------------------------------

interface CoverageChipProps {
  count: number | null | undefined;
  total: number | null | undefined;
}

function CoverageChip({ count, total }: CoverageChipProps) {
  if (count == null) {
    return (
      <Badge variant="outline" className="text-muted-foreground">
        —
      </Badge>
    );
  }
  const label = total != null ? `${count} / ${total}` : String(count);
  const isGood = total != null && count > 0;
  return (
    <Badge
      variant="secondary"
      className={cn(
        "tabular-nums",
        isGood
          ? "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
          : "text-muted-foreground",
      )}
      aria-label={`Coverage: ${label}`}
    >
      {label}
    </Badge>
  );
}
