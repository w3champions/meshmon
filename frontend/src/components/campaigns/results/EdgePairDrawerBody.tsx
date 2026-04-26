/**
 * EdgePairDrawerBody — pair-detail body for edge_candidate-mode candidates.
 *
 * Renders the per-(X, B) detail table driven by `useEdgePairDetails` with
 * `candidate_ip` filter. Columns: B name (CandidateRef inline), best_route_ms,
 * route shape chip, loss, stddev, qualifies indicator.
 *
 * Self-pair exclusion note (spec §5.5 edge case G-5): when the candidate
 * `is_mesh_member === true` AND its `agent_id` is in `campaign.source_agent_ids`,
 * an info note renders above the table: "Self-pair excluded — this candidate is
 * also a source agent in this campaign."
 */

import { useMemo } from "react";
import type { Campaign } from "@/api/hooks/campaigns";
import type { EvaluationEdgePairDetailDto } from "@/api/hooks/evaluation";
import { useEdgePairDetails } from "@/api/hooks/evaluation";
import type { components } from "@/api/schema.gen";
import { CandidateRef } from "@/components/campaigns/CandidateRef";
import { RouteLegRow } from "@/components/campaigns/results/RouteLegRow";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";

type EvaluationCandidateDto = components["schemas"]["EvaluationCandidateDto"];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface EdgePairDrawerBodyProps {
  candidateIp: string;
  candidate: EvaluationCandidateDto;
  campaign: Campaign;
}

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
// Component
// ---------------------------------------------------------------------------

export function EdgePairDrawerBody({
  candidateIp,
  candidate,
  campaign,
}: EdgePairDrawerBodyProps) {
  const hook = useEdgePairDetails(campaign.id, {
    candidate_ip: candidateIp,
  });

  const rows = useMemo<EvaluationEdgePairDetailDto[]>(
    () => hook.data?.pages.flatMap((p) => p.entries) ?? [],
    [hook.data],
  );

  // Self-pair exclusion check (spec §5.5 G-5):
  // The note shows when candidate is a mesh member AND its agent_id matches
  // a campaign source agent. Note: CampaignDto.source_agent_ids is optional.
  const sourceAgentIds = (campaign as Campaign & { source_agent_ids?: string[] }).source_agent_ids;
  const isSelfPairExcluded =
    candidate.is_mesh_member &&
    candidate.agent_id != null &&
    Array.isArray(sourceAgentIds) &&
    sourceAgentIds.includes(candidate.agent_id);

  return (
    <div className="flex-1 overflow-auto px-4 py-3" data-testid="edge-pair-drawer-body">
      {isSelfPairExcluded ? (
        <Card
          className="mb-3 border-blue-500/40 bg-blue-500/5 p-3 text-sm"
          role="note"
          data-testid="self-pair-excluded-note"
        >
          <span className="font-medium">Self-pair excluded</span> — this candidate is also a
          source agent in this campaign.
        </Card>
      ) : null}

      {hook.isLoading ? (
        <Card
          className="p-4 text-sm text-muted-foreground"
          role="status"
          aria-busy="true"
          data-testid="edge-pair-loading"
        >
          Loading edge pair details…
        </Card>
      ) : hook.isError ? (
        <Card className="border-destructive/50 bg-destructive/5 p-4 text-sm" role="alert">
          <p className="mb-2">
            <strong>Failed to load edge pair details.</strong>{" "}
            {hook.error?.message ?? "Unknown error."}
          </p>
          <Button type="button" size="sm" variant="outline" onClick={() => hook.refetch()}>
            Retry
          </Button>
        </Card>
      ) : rows.length === 0 ? (
        <Card className="p-4 text-sm text-muted-foreground" role="status">
          No edge pair detail rows found for this candidate.
        </Card>
      ) : (
        <div className="flex flex-col gap-0 rounded-md border" role="table" aria-label="Edge pair details">
          {/* Header */}
          <div
            role="row"
            className="grid w-full border-b bg-muted/30 px-3 py-2 text-xs font-medium uppercase tracking-wide text-muted-foreground"
            style={{ gridTemplateColumns: "minmax(180px,1fr) 110px 100px 90px 90px 90px" }}
          >
            <div role="columnheader" className="px-2">B (Destination)</div>
            <div role="columnheader" className="px-2 text-right">Best RTT</div>
            <div role="columnheader" className="px-2">Route</div>
            <div role="columnheader" className="px-2 text-right">Loss</div>
            <div role="columnheader" className="px-2 text-right">Stddev</div>
            <div role="columnheader" className="px-2">Status</div>
          </div>

          {/* Rows */}
          {rows.map((row, idx) => (
            <EdgePairRow
              key={`${row.candidate_ip}::${row.destination_agent_id}`}
              row={row}
              index={idx}
              lossThresholdRatio={campaign.loss_threshold_ratio}
            />
          ))}
        </div>
      )}

      {/* Load more */}
      {hook.hasNextPage ? (
        <div className="mt-2 flex justify-center">
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => { void hook.fetchNextPage(); }}
            disabled={hook.isFetchingNextPage}
          >
            {hook.isFetchingNextPage ? "Loading…" : "Load more"}
          </Button>
        </div>
      ) : null}
    </div>
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
    hostname: row.destination_hostname,
  };

  return (
    <div
      role="row"
      data-testid={`edge-pair-row-${index}`}
      className="border-b last:border-0"
    >
      <div
        className="grid w-full items-start px-3 py-2 text-sm hover:bg-muted/40"
        style={{ gridTemplateColumns: "minmax(180px,1fr) 110px 100px 90px 90px 90px" }}
      >
        {/* B name */}
        <div role="cell" className="px-2">
          <CandidateRef mode="inline" data={candidateRefData} />
          <div className="font-mono text-xs text-muted-foreground">{row.destination_agent_id}</div>
        </div>

        {/* Best RTT — `best_route_ms` is `null` for unreachable rows. */}
        <div role="cell" className="px-2 text-right tabular-nums">
          {row.is_unreachable || row.best_route_ms == null ? (
            <span className="text-muted-foreground">unreachable</span>
          ) : (
            formatMs(row.best_route_ms)
          )}
        </div>

        {/* Route shape chip */}
        <div role="cell" className="px-2">
          <Badge
            variant="outline"
            className={routeKindClass(row.best_route_kind)}
          >
            {routeKindLabel(row.best_route_kind)}
          </Badge>
        </div>

        {/* Loss */}
        <div role="cell" className="px-2 text-right tabular-nums text-xs text-muted-foreground">
          {formatLoss(row.best_route_loss_ratio)}
        </div>

        {/* Stddev */}
        <div role="cell" className="px-2 text-right tabular-nums text-xs text-muted-foreground">
          {formatMs(row.best_route_stddev_ms)}
        </div>

        {/* Qualifies indicator */}
        <div role="cell" className="px-2">
          {row.is_unreachable ? (
            <Badge variant="outline" className="text-destructive">unreachable</Badge>
          ) : row.qualifies_under_t ? (
            <Badge variant="secondary" className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-300">
              qualifies
            </Badge>
          ) : (
            <Badge variant="outline">above T</Badge>
          )}
        </div>
      </div>

      {/* Per-leg breakdown */}
      {row.best_route_legs.length > 0 ? (
        <div className="px-5 pb-2 border-t bg-muted/10">
          {row.best_route_legs.map((leg, legIdx) => (
            <RouteLegRow
              key={`${row.destination_agent_id}-leg-${legIdx}`}
              leg={leg}
              lossThresholdRatio={lossThresholdRatio}
            />
          ))}
        </div>
      ) : null}
    </div>
  );
}
