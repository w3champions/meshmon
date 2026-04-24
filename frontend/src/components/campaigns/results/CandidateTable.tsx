/**
 * Candidate results table for the `/campaigns/:id` Candidates tab.
 *
 * Renders a summary KPI strip followed by a sortable table over
 * `EvaluationDto.results.candidates`. Typical result sets are small (<100
 * rows), so there's no virtualization — a plain `<table>` wins on clarity
 * and keeps the markup screen-reader-friendly.
 *
 * Sign convention (spec §5): `improvement_ms > 0` means the A→X→B transit
 * is faster than the direct A→B baseline. Positive values render green,
 * negatives red. Never invert.
 */

import { useMemo } from "react";
import type { Evaluation } from "@/api/hooks/evaluation";
import { IpHostname } from "@/components/ip-hostname";
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
import { formatLossRatio, LOSS_RATIO_THRESHOLDS } from "@/lib/format-loss";
import { cn } from "@/lib/utils";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type Candidate = Evaluation["results"]["candidates"][number];

export type CandidateSortColumn =
  | "display_name"
  | "destination_ip"
  | "city"
  | "asn"
  | "pairs_improved"
  | "avg_improvement_ms"
  | "avg_loss_ratio"
  | "composite_score";

export type SortDirection = "asc" | "desc";

export interface CandidateTableSort {
  col: CandidateSortColumn;
  dir: SortDirection;
}

export interface CandidateTableProps {
  evaluation: Evaluation;
  /** Highlighted row — typically the candidate backing the open drawer. */
  selectedIp?: string | null;
  onSelectCandidate: (ip: string) => void;
  /** Optional row-action slot, rendered into each row's trailing cell. */
  renderRowActions?: (candidate: Candidate) => React.ReactNode;
  /** Current sort column + direction (driven by URL state). */
  sort: CandidateTableSort;
  onSortChange: (next: CandidateTableSort) => void;
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

function formatImprovement(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  const rounded = Math.round(value * 10) / 10;
  const sign = rounded > 0 ? "+" : "";
  return `${sign}${rounded.toFixed(1)} ms`;
}

function improvementClass(value: number | null | undefined): string {
  if (value === null || value === undefined) return "text-muted-foreground";
  if (value > 0) return "text-emerald-600 dark:text-emerald-400 font-medium";
  if (value < 0) return "text-destructive font-medium";
  return "text-muted-foreground";
}

/**
 * Class for a candidate's `avg_loss_ratio` badge. Both `value` and `threshold`
 * are wire-format ratios (0.0–1.0) — the per-campaign `loss_threshold_ratio`
 * is directly comparable without rescaling.
 */
function lossClass(value: number | null | undefined, threshold: number): string {
  if (value === null || value === undefined) return "";
  if (value < LOSS_RATIO_THRESHOLDS.healthy)
    return "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300";
  if (value < threshold) return "bg-amber-500/15 text-amber-700 dark:text-amber-300";
  return "bg-destructive/15 text-destructive";
}

function formatScore(value: number): string {
  return value.toFixed(2);
}

function asnLabel(candidate: Candidate): string {
  if (candidate.asn === null || candidate.asn === undefined) return "Unknown";
  const op = candidate.network_operator ?? "";
  return op ? `AS${candidate.asn} · ${op}` : `AS${candidate.asn}`;
}

function cityLabel(candidate: Candidate): string {
  const parts = [candidate.city, candidate.country_code].filter(
    (part): part is string => typeof part === "string" && part.length > 0,
  );
  return parts.length ? parts.join(", ") : "—";
}

// ---------------------------------------------------------------------------
// Sort
// ---------------------------------------------------------------------------

function compareByColumn(a: Candidate, b: Candidate, col: CandidateSortColumn): number {
  switch (col) {
    case "composite_score":
      return a.composite_score - b.composite_score;
    case "display_name":
      return (a.display_name ?? a.destination_ip).localeCompare(b.display_name ?? b.destination_ip);
    case "destination_ip":
      return a.destination_ip.localeCompare(b.destination_ip);
    case "city":
      return cityLabel(a).localeCompare(cityLabel(b));
    case "asn":
      return (a.asn ?? Number.MAX_SAFE_INTEGER) - (b.asn ?? Number.MAX_SAFE_INTEGER);
    case "pairs_improved":
      return a.pairs_improved - b.pairs_improved;
    case "avg_improvement_ms":
      return (a.avg_improvement_ms ?? -Infinity) - (b.avg_improvement_ms ?? -Infinity);
    case "avg_loss_ratio":
      return (a.avg_loss_ratio ?? Infinity) - (b.avg_loss_ratio ?? Infinity);
  }
}

function sortCandidates(rows: Candidate[], sort: CandidateTableSort): Candidate[] {
  const multiplier = sort.dir === "asc" ? 1 : -1;
  // Stable sort: indices break ties so repeated toggles don't scramble rows.
  const indexed = rows.map((row, idx) => ({ row, idx }));
  indexed.sort((a, b) => {
    const primary = compareByColumn(a.row, b.row, sort.col);
    if (primary !== 0) return primary * multiplier;
    return a.idx - b.idx;
  });
  return indexed.map(({ row }) => row);
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

interface SortableHeadProps {
  column: CandidateSortColumn;
  label: string;
  sort: CandidateTableSort;
  onSortChange: (next: CandidateTableSort) => void;
  className?: string;
}

function SortableHead({ column, label, sort, onSortChange, className }: SortableHeadProps) {
  const active = sort.col === column;
  const nextDir: SortDirection = active && sort.dir === "desc" ? "asc" : "desc";
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

export function CandidateTable({
  evaluation,
  selectedIp,
  onSelectCandidate,
  renderRowActions,
  sort,
  onSortChange,
}: CandidateTableProps) {
  const rows = useMemo(
    () => sortCandidates(evaluation.results.candidates, sort),
    [evaluation.results.candidates, sort],
  );

  const threshold = evaluation.loss_threshold_ratio;

  return (
    <div className="flex flex-col gap-3">
      <KpiStrip evaluation={evaluation} />

      {rows.length === 0 ? (
        <Card className="p-6 text-sm text-muted-foreground" role="status">
          No candidates matched the evaluator&apos;s scoring pass.
        </Card>
      ) : (
        <Card className="overflow-hidden">
          <Table aria-label="Evaluation candidates">
            <TableHeader>
              <TableRow>
                {/* The `#` column renders the position index (1, 2, …) as
                    the visual rank, but sorts by `composite_score` — that's
                    the signal the rank reflects. Wiring it to `rank` as a
                    separate sort key would duplicate the Score column and
                    mislead aria-sort consumers about which header is active. */}
                <SortableHead
                  column="composite_score"
                  label="#"
                  sort={sort}
                  onSortChange={onSortChange}
                  className="w-10"
                />
                <SortableHead
                  column="display_name"
                  label="Candidate"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                <SortableHead
                  column="destination_ip"
                  label="IP"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                <SortableHead
                  column="city"
                  label="Location"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                <SortableHead
                  column="asn"
                  label="ASN / operator"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                <SortableHead
                  column="pairs_improved"
                  label="Pairs"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                <SortableHead
                  column="avg_improvement_ms"
                  label="Δ avg"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                <SortableHead
                  column="avg_loss_ratio"
                  label="Loss"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                <SortableHead
                  column="composite_score"
                  label="Score"
                  sort={sort}
                  onSortChange={onSortChange}
                />
                {renderRowActions ? <TableHead className="w-10" aria-label="Actions" /> : null}
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((candidate, index) => {
                const isSelected = selectedIp === candidate.destination_ip;
                return (
                  <TableRow
                    key={candidate.destination_ip}
                    data-state={isSelected ? "selected" : undefined}
                    data-testid={`candidate-row-${candidate.destination_ip}`}
                    className="cursor-pointer"
                    onClick={() => onSelectCandidate(candidate.destination_ip)}
                  >
                    <TableCell className="text-muted-foreground">{index + 1}</TableCell>
                    <TableCell>
                      <div className="flex items-center gap-2">
                        <span className="font-medium">
                          {candidate.display_name ?? candidate.destination_ip}
                        </span>
                        {candidate.is_mesh_member ? (
                          <Badge
                            variant="secondary"
                            aria-label="Mesh member — no acquisition needed"
                          >
                            mesh
                          </Badge>
                        ) : null}
                      </div>
                    </TableCell>
                    <TableCell className="text-xs">
                      <IpHostname ip={candidate.destination_ip} />
                    </TableCell>
                    <TableCell className="text-sm">{cityLabel(candidate)}</TableCell>
                    <TableCell className="text-sm">
                      {candidate.asn === null || candidate.asn === undefined ? (
                        <Badge variant="outline">Unknown</Badge>
                      ) : (
                        <span>{asnLabel(candidate)}</span>
                      )}
                    </TableCell>
                    <TableCell className="text-sm tabular-nums">
                      {candidate.pairs_improved} / {candidate.pairs_total_considered}
                    </TableCell>
                    <TableCell
                      className={cn(
                        "text-sm tabular-nums",
                        improvementClass(candidate.avg_improvement_ms),
                      )}
                    >
                      {formatImprovement(candidate.avg_improvement_ms)}
                    </TableCell>
                    <TableCell>
                      <Badge
                        variant="outline"
                        className={cn(
                          "font-mono text-xs",
                          lossClass(candidate.avg_loss_ratio, threshold),
                        )}
                      >
                        {formatLossRatio(candidate.avg_loss_ratio)}
                      </Badge>
                    </TableCell>
                    <TableCell className="text-sm tabular-nums">
                      {formatScore(candidate.composite_score)}
                    </TableCell>
                    {renderRowActions ? (
                      <TableCell onClick={(event) => event.stopPropagation()}>
                        {renderRowActions(candidate)}
                      </TableCell>
                    ) : null}
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
// KPI strip
// ---------------------------------------------------------------------------

interface KpiStripProps {
  evaluation: Evaluation;
}

function KpiStrip({ evaluation }: KpiStripProps) {
  const avgImprovement = evaluation.avg_improvement_ms;
  return (
    <section aria-label="Evaluation summary" className="grid grid-cols-2 gap-3 sm:grid-cols-4">
      <KpiCard label="Baseline pairs" value={evaluation.baseline_pair_count.toLocaleString()} />
      <KpiCard label="Candidates" value={evaluation.candidates_total.toLocaleString()} />
      <KpiCard
        label="Good candidates"
        value={`${evaluation.candidates_good.toLocaleString()} / ${evaluation.candidates_total.toLocaleString()}`}
      />
      <KpiCard
        label="Avg improvement"
        value={formatImprovement(avgImprovement)}
        valueClassName={improvementClass(avgImprovement)}
      />
    </section>
  );
}

interface KpiCardProps {
  label: string;
  value: string;
  valueClassName?: string;
}

function KpiCard({ label, value, valueClassName }: KpiCardProps) {
  return (
    <Card className="flex flex-col gap-1 p-4">
      <span className="text-xs uppercase tracking-wide text-muted-foreground">{label}</span>
      <span className={cn("text-2xl font-semibold tabular-nums", valueClassName)}>{value}</span>
    </Card>
  );
}
