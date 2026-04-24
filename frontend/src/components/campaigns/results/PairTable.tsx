/**
 * Pivot table for the Pairs tab — one row per `(source_agent_id,
 * destination_ip)` baseline pair.
 *
 * Complements the Candidates tab's transit-centric view: this table is
 * source-pair-centric and surfaces every configured baseline regardless of
 * whether the evaluator scored it. Rows carry resolution state, dispatch
 * timing, attempt counts, and per-row actions ("force re-measure", "dispatch
 * detail for this pair"). Sorting is column-local; the operator picks a
 * header and the rows re-order in-place without a round-trip.
 */

import { useMemo, useState } from "react";
import type { AgentSummary } from "@/api/hooks/agents";
import type { CampaignPair, PairResolutionState, ProbeProtocol } from "@/api/hooks/campaigns";
import { IpHostname } from "@/components/ip-hostname";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  measurementSourceBadgeClass,
  measurementSourceLabel,
  normaliseSource,
} from "@/lib/measurement-source";
import { cn } from "@/lib/utils";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type PairSortColumn =
  | "source"
  | "destination"
  | "state"
  | "dispatched_at"
  | "settled_at"
  | "attempt_count";

export type SortDirection = "asc" | "desc";

export interface PairTableSort {
  col: PairSortColumn;
  dir: SortDirection;
}

export interface PairRowAction {
  source_agent_id: string;
  destination_ip: string;
}

export interface PairTableProps {
  pairs: CampaignPair[];
  /** Owner of this campaign's probe protocol, surfaced in the protocol column. */
  protocol: ProbeProtocol;
  /** Agent id → summary lookup; resolves source IDs into display names. */
  agentsById: Map<string, AgentSummary>;
  onForcePair: (pair: PairRowAction) => void;
  onTriggerPairDetail: (pair: PairRowAction) => void;
  sort: PairTableSort;
  onSortChange: (next: PairTableSort) => void;
}

// ---------------------------------------------------------------------------
// Formatting + sort helpers
// ---------------------------------------------------------------------------

const STATE_BADGE_CLASS: Record<PairResolutionState, string> = {
  pending: "bg-muted text-muted-foreground hover:bg-muted",
  dispatched: "bg-blue-500/15 text-blue-700 dark:text-blue-300 hover:bg-blue-500/20",
  reused: "bg-cyan-500/15 text-cyan-700 dark:text-cyan-300 hover:bg-cyan-500/20",
  succeeded: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300 hover:bg-emerald-500/20",
  unreachable: "bg-destructive/15 text-destructive hover:bg-destructive/20",
  skipped: "bg-amber-500/15 text-amber-700 dark:text-amber-300 hover:bg-amber-500/20",
};

// Sort order for the `State` column: lifecycle (pending → dispatched →
// terminal successes → terminal failures) rather than alphabetical, so
// asc-sort surfaces work in progress at the top and desc-sort surfaces
// terminal failure states first — which is the shape operators scan for.
const STATE_ORDINAL: Record<PairResolutionState, number> = {
  pending: 0,
  dispatched: 1,
  reused: 2,
  succeeded: 3,
  unreachable: 4,
  skipped: 5,
};

function sourceLabel(agentsById: Map<string, AgentSummary>, agentId: string): string {
  return agentsById.get(agentId)?.display_name || agentId;
}

function formatTimestamp(value: string | null | undefined): string {
  return value ?? "—";
}

function compareByColumn(
  a: CampaignPair,
  b: CampaignPair,
  col: PairSortColumn,
  agentsById: Map<string, AgentSummary>,
): number {
  switch (col) {
    case "source":
      return sourceLabel(agentsById, a.source_agent_id).localeCompare(
        sourceLabel(agentsById, b.source_agent_id),
      );
    case "destination":
      return a.destination_ip.localeCompare(b.destination_ip);
    case "state":
      return STATE_ORDINAL[a.resolution_state] - STATE_ORDINAL[b.resolution_state];
    case "dispatched_at":
      return (a.dispatched_at ?? "").localeCompare(b.dispatched_at ?? "");
    case "settled_at":
      return (a.settled_at ?? "").localeCompare(b.settled_at ?? "");
    case "attempt_count":
      return a.attempt_count - b.attempt_count;
  }
}

function sortPairs(
  rows: CampaignPair[],
  sort: PairTableSort,
  agentsById: Map<string, AgentSummary>,
): CampaignPair[] {
  const multiplier = sort.dir === "asc" ? 1 : -1;
  const indexed = rows.map((row, idx) => ({ row, idx }));
  indexed.sort((a, b) => {
    const primary = compareByColumn(a.row, b.row, sort.col, agentsById);
    if (primary !== 0) return primary * multiplier;
    // Stable fallback so repeat toggles don't scramble equal-key rows.
    return a.idx - b.idx;
  });
  return indexed.map(({ row }) => row);
}

// ---------------------------------------------------------------------------
// Sortable header
// ---------------------------------------------------------------------------

interface SortableHeadProps {
  column: PairSortColumn;
  label: string;
  sort: PairTableSort;
  onSortChange: (next: PairTableSort) => void;
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

// ---------------------------------------------------------------------------
// Row-level action menu
// ---------------------------------------------------------------------------

interface RowMenuProps {
  pair: CampaignPair;
  onForcePair: (pair: PairRowAction) => void;
  onTriggerPairDetail: (pair: PairRowAction) => void;
}

function RowMenu({ pair, onForcePair, onTriggerPairDetail }: RowMenuProps) {
  const action: PairRowAction = {
    source_agent_id: pair.source_agent_id,
    destination_ip: pair.destination_ip,
  };
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          aria-label={`Actions for pair ${pair.source_agent_id} → ${pair.destination_ip}`}
        >
          ⋯
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem onSelect={() => onForcePair(action)}>
          Force re-measure pair
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => onTriggerPairDetail(action)}>
          Dispatch detail for this pair
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function PairTable({
  pairs,
  protocol,
  agentsById,
  onForcePair,
  onTriggerPairDetail,
  sort,
  onSortChange,
}: PairTableProps) {
  const rows = useMemo(() => sortPairs(pairs, sort, agentsById), [pairs, sort, agentsById]);

  if (rows.length === 0) {
    return (
      <Card className="p-6 text-sm text-muted-foreground" role="status">
        No pairs on this campaign yet.
      </Card>
    );
  }

  return (
    <Card className="overflow-hidden">
      <Table aria-label="Campaign pairs">
        <TableHeader>
          <TableRow>
            <SortableHead column="source" label="Source" sort={sort} onSortChange={onSortChange} />
            <SortableHead
              column="destination"
              label="Destination"
              sort={sort}
              onSortChange={onSortChange}
            />
            <SortableHead column="state" label="State" sort={sort} onSortChange={onSortChange} />
            <TableHead>Protocol</TableHead>
            <TableHead>Source</TableHead>
            <SortableHead
              column="dispatched_at"
              label="Dispatched"
              sort={sort}
              onSortChange={onSortChange}
            />
            <SortableHead
              column="settled_at"
              label="Settled"
              sort={sort}
              onSortChange={onSortChange}
            />
            <SortableHead
              column="attempt_count"
              label="Attempts"
              sort={sort}
              onSortChange={onSortChange}
              className="w-24"
            />
            <TableHead>Last error</TableHead>
            <TableHead className="w-10" aria-label="Actions" />
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((pair) => {
            return (
              <TableRow
                key={pair.id}
                data-testid={`pair-row-${pair.id}`}
                data-pair-source={pair.source_agent_id}
                data-pair-destination={pair.destination_ip}
              >
                <TableCell>
                  <div className="flex flex-col">
                    <span className="font-medium">
                      {sourceLabel(agentsById, pair.source_agent_id)}
                    </span>
                    <span className="font-mono text-xs text-muted-foreground">
                      {pair.source_agent_id}
                    </span>
                  </div>
                </TableCell>
                <TableCell className="text-xs">
                  <IpHostname ip={pair.destination_ip} />
                </TableCell>
                <TableCell>
                  <Badge
                    className={cn(STATE_BADGE_CLASS[pair.resolution_state])}
                    aria-label={`State: ${pair.resolution_state}`}
                  >
                    {pair.resolution_state}
                  </Badge>
                </TableCell>
                <TableCell className="text-xs uppercase tracking-wide">{protocol}</TableCell>
                <TableCell>
                  <SourceBadge source={pair.source} />
                </TableCell>
                <TableCell className="font-mono text-xs text-muted-foreground">
                  {formatTimestamp(pair.dispatched_at)}
                </TableCell>
                <TableCell className="font-mono text-xs text-muted-foreground">
                  {formatTimestamp(pair.settled_at)}
                </TableCell>
                <TableCell className="tabular-nums">{pair.attempt_count}</TableCell>
                <LastErrorCell error={pair.last_error} />
                <TableCell>
                  <RowMenu
                    pair={pair}
                    onForcePair={onForcePair}
                    onTriggerPairDetail={onTriggerPairDetail}
                  />
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// Source badge
// ---------------------------------------------------------------------------

interface SourceBadgeProps {
  source: CampaignPair["source"] | null | undefined;
}

/**
 * Visual chip distinguishing `archived_vm_continuous` pairs (VM-archived
 * baselines pulled from continuous-mesh data) from `active_probe` pairs.
 * Sky/blue tone for VM baselines; muted tone for active-probe rows so the
 * default case doesn't add visual noise.
 */
function SourceBadge({ source }: SourceBadgeProps) {
  const resolved = normaliseSource(source);
  return (
    <Badge
      variant="outline"
      className={cn(
        "font-mono text-[10px] uppercase tracking-wide",
        measurementSourceBadgeClass(resolved),
      )}
      data-testid={`pair-source-badge-${resolved}`}
      aria-label={`Source: ${measurementSourceLabel(resolved)}`}
    >
      {measurementSourceLabel(resolved)}
    </Badge>
  );
}

// ---------------------------------------------------------------------------
// Last-error cell
// ---------------------------------------------------------------------------

interface LastErrorCellProps {
  error: string | null | undefined;
}

/**
 * `title` attribute renders a native tooltip so the full error is visible on
 * hover without pulling in the Radix tooltip primitive — this table already
 * carries enough Radix dependencies via the dropdown menu; keeping error
 * inspection lightweight avoids a second floating-ui instance per row.
 */
function LastErrorCell({ error }: LastErrorCellProps) {
  if (!error) {
    return <TableCell className="text-xs text-muted-foreground">—</TableCell>;
  }
  return (
    <TableCell className="max-w-[220px] truncate text-xs text-destructive" title={error}>
      {error}
    </TableCell>
  );
}

// ---------------------------------------------------------------------------
// State-local sort hook
// ---------------------------------------------------------------------------

/**
 * Component-local sort state for the Pairs tab. The current plan does not
 * require URL persistence here (unlike the Candidates tab's `cand_*` keys),
 * so a plain `useState` keeps the tab shell simple and leaves room for a
 * `pair_*` URL prefix later without breaking the API.
 */
export function usePairTableSort(
  initial: PairTableSort,
): [PairTableSort, (next: PairTableSort) => void] {
  const [sort, setSort] = useState<PairTableSort>(initial);
  return [sort, setSort];
}
