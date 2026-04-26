/**
 * EdgePairsTab — flat (X, B) pivot table for edge_candidate mode.
 *
 * Renders every (candidate, destination) pair across the whole campaign
 * (no candidate_ip filter). Backed by `useEdgePairDetails` with URL-encoded
 * sort state (`ep_sort` / `ep_dir` search params).
 *
 * Columns (spec §6.5): X candidate (CandidateRef inline), B destination
 * agent id, best_route_ms, route shape chip, loss, stddev, qualifies.
 */

import { useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useMemo } from "react";
import type { Campaign } from "@/api/hooks/campaigns";
import type { EdgePairsSortCol, EdgePairsSortDir } from "@/api/hooks/campaigns";
import type { EvaluationEdgePairDetailDto } from "@/api/hooks/evaluation";
import { useEdgePairDetails } from "@/api/hooks/evaluation";
import { CandidateRef } from "@/components/campaigns/CandidateRef";
import { RouteLegRow } from "@/components/campaigns/results/RouteLegRow";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import type { CampaignDetailSearch } from "@/router/index";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface EdgePairsTabProps {
  campaign: Campaign;
}

const DEFAULT_EP_SORT: EdgePairsSortCol = "best_route_ms";
const DEFAULT_EP_DIR: EdgePairsSortDir = "asc";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function routeKindLabel(kind: EvaluationEdgePairDetailDto["best_route_kind"]): string {
  switch (kind) {
    case "direct":
      return "direct";
    case "one_hop":
      return "1 hop";
    case "two_hop":
      return "2 hops";
    default:
      return kind;
  }
}

function routeKindClass(kind: EvaluationEdgePairDetailDto["best_route_kind"]): string {
  switch (kind) {
    case "direct":
      return "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300";
    case "one_hop":
      return "bg-blue-500/15 text-blue-700 dark:text-blue-300";
    case "two_hop":
      return "bg-amber-500/15 text-amber-700 dark:text-amber-300";
    default:
      return "";
  }
}

function formatMs(ms: number): string {
  return `${ms.toFixed(1)} ms`;
}

function formatLoss(ratio: number): string {
  if (ratio === 0) return "0 %";
  return `${(ratio * 100).toFixed(2)} %`;
}

// ---------------------------------------------------------------------------
// Sortable column header
// ---------------------------------------------------------------------------

interface SortableHeadProps {
  col: EdgePairsSortCol;
  label: string;
  activeCol: EdgePairsSortCol;
  activeDir: EdgePairsSortDir;
  onSort: (col: EdgePairsSortCol, dir: EdgePairsSortDir) => void;
  className?: string;
}

function SortableHead({ col, label, activeCol, activeDir, onSort, className }: SortableHeadProps) {
  const isActive = activeCol === col;
  const nextDir: EdgePairsSortDir = isActive && activeDir === "desc" ? "asc" : "desc";
  const ariaSort = isActive ? (activeDir === "asc" ? "ascending" : "descending") : "none";

  return (
    <TableHead className={className} aria-sort={ariaSort}>
      <button
        type="button"
        className="flex items-center gap-1 text-left font-medium hover:text-foreground"
        onClick={() => onSort(col, nextDir)}
      >
        {label}
        <span aria-hidden className="text-xs text-muted-foreground">
          {isActive ? (activeDir === "asc" ? "▲" : "▼") : "↕"}
        </span>
      </button>
    </TableHead>
  );
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function EdgePairsTab({ campaign }: EdgePairsTabProps) {
  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as CampaignDetailSearch;

  const sortCol = (search.ep_sort ?? DEFAULT_EP_SORT) as EdgePairsSortCol;
  const sortDir = (search.ep_dir ?? DEFAULT_EP_DIR) as EdgePairsSortDir;

  const setSort = useCallback(
    (col: EdgePairsSortCol, dir: EdgePairsSortDir): void => {
      const navigateSearch = navigate as unknown as (opts: {
        search: CampaignDetailSearch;
        replace: boolean;
      }) => void;
      navigateSearch({
        search: { ...search, ep_sort: col, ep_dir: dir },
        replace: true,
      });
    },
    [navigate, search],
  );

  const query = useEdgePairDetails(campaign.id, useMemo(
    () => ({ sort: sortCol, dir: sortDir }),
    [sortCol, sortDir],
  ));

  const rows = useMemo<EvaluationEdgePairDetailDto[]>(
    () => query.data?.pages.flatMap((p) => p.entries) ?? [],
    [query.data],
  );

  // ------------------------------------------------------------------
  // Loading / error
  // ------------------------------------------------------------------

  if (query.isLoading) {
    return (
      <section
        data-testid="edge-pairs-tab"
        role="status"
        aria-live="polite"
        className="flex flex-col gap-3"
      >
        <span className="sr-only">Loading edge pairs…</span>
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-64 w-full" />
      </section>
    );
  }

  if (query.isError) {
    return (
      <section data-testid="edge-pairs-tab" className="flex flex-col gap-3">
        <Card className="p-4 text-sm text-destructive" role="alert">
          Failed to load edge pairs: {query.error?.message ?? "unknown error"}
        </Card>
      </section>
    );
  }

  // ------------------------------------------------------------------
  // Empty
  // ------------------------------------------------------------------

  if (rows.length === 0) {
    return (
      <section data-testid="edge-pairs-tab" className="flex flex-col gap-3">
        <Card className="p-6 text-sm text-muted-foreground" role="status">
          No edge pair data for this campaign yet.
        </Card>
      </section>
    );
  }

  // ------------------------------------------------------------------
  // Table
  // ------------------------------------------------------------------

  return (
    <section data-testid="edge-pairs-tab" className="flex flex-col gap-4">
      <Card className="overflow-hidden">
        <Table aria-label="Edge pairs">
          <TableHeader>
            <TableRow>
              <TableHead>X (Candidate)</TableHead>
              <SortableHead
                col="destination_agent_id"
                label="B (Destination)"
                activeCol={sortCol}
                activeDir={sortDir}
                onSort={setSort}
              />
              <SortableHead
                col="best_route_ms"
                label="Best RTT"
                activeCol={sortCol}
                activeDir={sortDir}
                onSort={setSort}
                className="text-right"
              />
              <SortableHead
                col="best_route_kind"
                label="Route"
                activeCol={sortCol}
                activeDir={sortDir}
                onSort={setSort}
              />
              <SortableHead
                col="best_route_loss_ratio"
                label="Loss"
                activeCol={sortCol}
                activeDir={sortDir}
                onSort={setSort}
                className="text-right"
              />
              <SortableHead
                col="best_route_stddev_ms"
                label="Stddev"
                activeCol={sortCol}
                activeDir={sortDir}
                onSort={setSort}
                className="text-right"
              />
              <SortableHead
                col="qualifies_under_t"
                label="Status"
                activeCol={sortCol}
                activeDir={sortDir}
                onSort={setSort}
              />
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row, idx) => (
              <EdgePairRow
                key={`${row.candidate_ip}::${row.destination_agent_id}`}
                row={row}
                index={idx}
                lossThresholdRatio={campaign.loss_threshold_ratio}
              />
            ))}
          </TableBody>
        </Table>
      </Card>

      {query.hasNextPage ? (
        <div className="flex justify-center">
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => { void query.fetchNextPage(); }}
            disabled={query.isFetchingNextPage}
            data-testid="edge-pairs-load-more"
          >
            {query.isFetchingNextPage ? "Loading…" : "Load more"}
          </Button>
        </div>
      ) : null}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Row
// ---------------------------------------------------------------------------

interface EdgePairRowProps {
  row: EvaluationEdgePairDetailDto;
  index: number;
  lossThresholdRatio: number;
}

function EdgePairRow({ row, index, lossThresholdRatio }: EdgePairRowProps) {
  const candidateRefData = {
    ip: row.candidate_ip,
    display_name: null,
    is_mesh_member: false,
    hostname: undefined,
  };

  const destRefData = {
    ip: row.destination_agent_id,
    display_name: null,
    is_mesh_member: false,
    hostname: row.destination_hostname,
  };

  return (
    <>
      <TableRow data-testid={`edge-pair-row-${index}`} className="align-top">
        {/* X — candidate */}
        <TableCell>
          <CandidateRef mode="inline" data={candidateRefData} />
          <div className="font-mono text-xs text-muted-foreground">{row.candidate_ip}</div>
        </TableCell>

        {/* B — destination agent id */}
        <TableCell>
          <CandidateRef mode="inline" data={destRefData} />
          <div className="font-mono text-xs text-muted-foreground">{row.destination_agent_id}</div>
        </TableCell>

        {/* Best RTT */}
        <TableCell className="text-right tabular-nums text-sm">
          {row.is_unreachable ? (
            <span className="text-muted-foreground">unreachable</span>
          ) : (
            formatMs(row.best_route_ms)
          )}
        </TableCell>

        {/* Route shape chip */}
        <TableCell>
          <Badge variant="outline" className={routeKindClass(row.best_route_kind)}>
            {routeKindLabel(row.best_route_kind)}
          </Badge>
        </TableCell>

        {/* Loss */}
        <TableCell className="text-right tabular-nums text-xs text-muted-foreground">
          {formatLoss(row.best_route_loss_ratio)}
        </TableCell>

        {/* Stddev */}
        <TableCell className="text-right tabular-nums text-xs text-muted-foreground">
          {formatMs(row.best_route_stddev_ms)}
        </TableCell>

        {/* Status */}
        <TableCell>
          {row.is_unreachable ? (
            <Badge variant="outline" className="text-destructive">
              unreachable
            </Badge>
          ) : row.qualifies_under_t ? (
            <Badge
              variant="secondary"
              className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
            >
              qualifies
            </Badge>
          ) : (
            <Badge variant="outline">above T</Badge>
          )}
        </TableCell>
      </TableRow>

      {/* Per-leg breakdown inline */}
      {row.best_route_legs.length > 0 ? (
        <TableRow className="bg-muted/10 hover:bg-muted/10">
          <TableCell colSpan={7} className="py-1 pl-6">
            {row.best_route_legs.map((leg, legIdx) => (
              <RouteLegRow
                key={`${row.candidate_ip}-${row.destination_agent_id}-leg-${legIdx}`}
                leg={leg}
                lossThresholdRatio={lossThresholdRatio}
              />
            ))}
          </TableCell>
        </TableRow>
      ) : null}
    </>
  );
}
