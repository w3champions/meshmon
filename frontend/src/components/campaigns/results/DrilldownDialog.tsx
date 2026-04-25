/**
 * Centered drilldown dialog for the Candidates tab.
 *
 * Centered modal (`max-w-6xl`, `max-h-[85vh]`, internal scroll). The
 * body paginates the candidate's pair-detail rows via
 * [`useCandidatePairDetails`], surfaces a sticky filter toolbar, and
 * (lazily) renders an inline `MtrPanel` below the table when an MTR
 * icon is clicked.
 *
 * Caption math: the dialog runs `useCandidatePairDetails` twice. The
 * first call mirrors the active toolbar state (drives the table); the
 * second clears every toolbar filter so its `pages[0].total` is the
 * full count of rows the *storage* filter let through. The caption
 * renders `Showing X of Y rows · Z hidden by storage guardrails`,
 * where `Z = candidate.pairs_total_considered − Y`. With no
 * guardrails active, `Z = 0` and the trailing clause is omitted.
 *
 * Cost: one extra `COUNT(*)` per dialog open for the unfiltered hook.
 * Justified — the alternative (a sidecar `total_dropped` field on
 * every paginated response) would persist a value that goes stale the
 * moment the operator changes a guardrail, and the bounded row set is
 * served by an indexed scan.
 */

import { useMemo, useState } from "react";
import { type AgentSummary, useAgents } from "@/api/hooks/agents";
import type { Campaign } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";
import {
  type EvaluationPairDetail,
  type PairDetailSortCol,
  type PairDetailsQuery,
  useCandidatePairDetails,
} from "@/api/hooks/evaluation-pairs";
import { CandidatePairFilters } from "@/components/campaigns/results/CandidatePairFilters";
import { CandidatePairTable } from "@/components/campaigns/results/CandidatePairTable";
import { MtrPanel } from "@/components/campaigns/results/MtrPanel";
import { IpHostname } from "@/components/ip-hostname";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { SortState } from "@/components/ui/sortable-header";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type Candidate = Evaluation["results"]["candidates"][number];

export interface DrilldownDialogProps {
  candidate: Candidate | null;
  campaign: Campaign;
  /**
   * Latest evaluation snapshot. Carries the active guardrail values
   * (rendered as input placeholders) and the candidate's headline
   * counters used by the caption math.
   */
  evaluation: Evaluation | null;
  /**
   * Unqualified-reason map off `EvaluationResultsDto.unqualified_reasons`;
   * rendered verbatim under the candidate header when present.
   */
  unqualifiedReason?: string;
  onClose: () => void;
}

const DEFAULT_QUERY: PairDetailsQuery = {
  sort: "improvement_ms",
  dir: "desc",
};

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function DrilldownDialog({
  candidate,
  campaign,
  evaluation,
  unqualifiedReason,
  onClose,
}: DrilldownDialogProps) {
  const open = candidate !== null;
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="flex max-h-[85vh] max-w-6xl flex-col gap-0 overflow-hidden p-0 sm:rounded-lg">
        {candidate ? (
          <DialogBody
            candidate={candidate}
            campaign={campaign}
            evaluation={evaluation}
            unqualifiedReason={unqualifiedReason}
            onClose={onClose}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// Body
// ---------------------------------------------------------------------------

interface DialogBodyProps {
  candidate: Candidate;
  campaign: Campaign;
  evaluation: Evaluation | null;
  unqualifiedReason: string | undefined;
  onClose: () => void;
}

function DialogBody({
  candidate,
  campaign,
  evaluation,
  unqualifiedReason,
  onClose,
}: DialogBodyProps) {
  const [query, setQuery] = useState<PairDetailsQuery>(DEFAULT_QUERY);
  const [activeMtr, setActiveMtr] = useState<{
    measurementId: number;
    label: string;
  } | null>(null);

  // Reset the inline MTR panel when the operator switches sort/filter
  // — the rows under the panel are about to change, so the lingering
  // measurement label would be stale.
  const sort = useMemo<SortState<PairDetailSortCol>>(
    () => ({ col: query.sort, dir: query.dir }),
    [query.sort, query.dir],
  );

  const onSortChange = (col: PairDetailSortCol | null, dir: "asc" | "desc" | null) => {
    if (col === null || dir === null) {
      // The three-state cycle's "none" rests on the default — we keep
      // the column but flip the direction back to desc so the table
      // never renders an empty sort indicator next to data.
      setQuery((prev) => ({ ...prev, ...DEFAULT_QUERY }));
    } else {
      setQuery((prev) => ({ ...prev, sort: col, dir }));
    }
    setActiveMtr(null);
  };

  const onFilterChange = (next: PairDetailsQuery) => {
    setQuery(next);
    setActiveMtr(null);
  };

  // Unfiltered count drives the caption math. We strip every toolbar
  // filter but keep the sort key — the row count is filter-invariant
  // under sort, so reusing the active sort lets TanStack Query share
  // the pages[0].total number across both hooks when the operator has
  // not narrowed yet.
  const unfilteredQuery = useMemo<PairDetailsQuery>(
    () => ({
      sort: query.sort,
      dir: query.dir,
      // limit=0 returns an empty entries array but the same `total`,
      // so we pay a single `COUNT(*)` round-trip and skip the row-
      // hydration cost when the operator opens the dialog.
      limit: 0,
    }),
    [query.sort, query.dir],
  );

  const filteredHook = useCandidatePairDetails(campaign.id, candidate.destination_ip, query);
  const unfilteredHook = useCandidatePairDetails(
    campaign.id,
    candidate.destination_ip,
    unfilteredQuery,
  );

  const agentsQuery = useAgents();
  const agentsById = useMemo<Map<string, AgentSummary>>(() => {
    const map = new Map<string, AgentSummary>();
    for (const agent of agentsQuery.data ?? []) {
      map.set(agent.id, agent);
    }
    return map;
  }, [agentsQuery.data]);

  const rows = useMemo<EvaluationPairDetail[]>(
    () => filteredHook.data?.pages.flatMap((p) => p.entries) ?? [],
    [filteredHook.data],
  );

  const filteredTotal = filteredHook.data?.pages[0]?.total ?? 0;
  const unfilteredTotal = unfilteredHook.data?.pages[0]?.total ?? 0;

  const guardrails = useMemo(
    () => ({
      min_improvement_ms: evaluation?.min_improvement_ms ?? null,
      min_improvement_ratio: evaluation?.min_improvement_ratio ?? null,
      max_transit_rtt_ms: evaluation?.max_transit_rtt_ms ?? null,
      max_transit_stddev_ms: evaluation?.max_transit_stddev_ms ?? null,
    }),
    [evaluation],
  );
  const guardrailActive = useMemo(
    () =>
      guardrails.min_improvement_ms !== null ||
      guardrails.min_improvement_ratio !== null ||
      guardrails.max_transit_rtt_ms !== null ||
      guardrails.max_transit_stddev_ms !== null,
    [guardrails],
  );

  const totalConsidered = candidate.pairs_total_considered;
  // Clamp to zero in case a stale unfiltered count slips above the
  // counter (e.g. an old SSE invalidation racing the read).
  const guardrailHidden = guardrailActive ? Math.max(0, totalConsidered - unfilteredTotal) : 0;

  const filterIsActive =
    query.min_improvement_ms != null ||
    query.min_improvement_ratio != null ||
    query.max_transit_rtt_ms != null ||
    query.max_transit_stddev_ms != null ||
    query.qualifies_only === true;

  const errorState = filteredHook.error;
  const is404NotACandidate =
    errorState?.cause &&
    typeof errorState.cause === "object" &&
    "error" in (errorState.cause as Record<string, unknown>) &&
    (errorState.cause as { error?: unknown }).error === "not_a_candidate";

  return (
    <>
      <DialogHeader className="border-b px-6 pb-4 pt-5">
        <DialogTitle className="flex flex-wrap items-center gap-2">
          {candidate.display_name ? (
            candidate.display_name
          ) : (
            <IpHostname ip={candidate.destination_ip} />
          )}
          {candidate.is_mesh_member ? (
            <Badge variant="secondary" aria-label="Mesh member">
              mesh
            </Badge>
          ) : null}
        </DialogTitle>
        <DialogDescription className="flex flex-wrap items-center gap-3">
          <span>
            Transit candidate <IpHostname ip={candidate.destination_ip} />
          </span>
          <span aria-hidden>·</span>
          <span>
            <strong className="tabular-nums">{candidate.pairs_improved}</strong> of{" "}
            <strong className="tabular-nums">{candidate.pairs_total_considered}</strong> baseline
            pairs improved
          </span>
          {guardrailActive ? (
            <Badge
              variant="outline"
              className="font-mono text-[10px]"
              aria-label="Active guardrails"
            >
              {summarizeGuardrails(guardrails)}
            </Badge>
          ) : null}
        </DialogDescription>
        {unqualifiedReason ? (
          <Card className="border-amber-500/50 bg-amber-500/5 p-3 text-sm" role="status">
            <span className="font-medium">Unqualified:</span> {unqualifiedReason}
          </Card>
        ) : null}
      </DialogHeader>

      <CandidatePairFilters value={query} onChange={onFilterChange} guardrails={guardrails} />

      <div className="flex-1 overflow-auto px-4 py-3" data-testid="drilldown-body">
        <p
          className="mb-2 text-xs text-muted-foreground"
          role="status"
          aria-live="polite"
          data-testid="drilldown-caption"
        >
          {captionText({
            filteredTotal,
            unfilteredTotal,
            guardrailHidden,
            guardrailActive,
            isLoading: filteredHook.isLoading || unfilteredHook.isLoading,
          })}
        </p>

        {is404NotACandidate ? (
          <Card className="border-destructive/50 bg-destructive/5 p-4 text-sm" role="alert">
            <p className="mb-2">
              <strong>Not a candidate.</strong> The latest evaluation does not list{" "}
              <IpHostname ip={candidate.destination_ip} /> as a transit candidate. Re-run the
              evaluator if the candidate set has changed.
            </p>
            <Button type="button" size="sm" variant="outline" onClick={onClose}>
              Close
            </Button>
          </Card>
        ) : filteredHook.isError ? (
          <Card className="border-destructive/50 bg-destructive/5 p-4 text-sm" role="alert">
            <p className="mb-2">
              <strong>Failed to load pair details.</strong>{" "}
              {filteredHook.error?.message ?? "Unknown error."}
            </p>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => filteredHook.refetch()}
            >
              Retry
            </Button>
          </Card>
        ) : filteredTotal === 0 && filterIsActive ? (
          <Card className="p-4 text-sm text-muted-foreground" role="status">
            No rows match these filters. Clear filters or re-evaluate with looser guardrails.
          </Card>
        ) : filteredTotal === 0 && !filterIsActive ? (
          <Card className="p-4 text-sm text-muted-foreground" role="status">
            All scored rows for this candidate were dropped by the active guardrails. Re-evaluate
            with looser guardrails to inspect them.
          </Card>
        ) : (
          <CandidatePairTable
            rows={rows}
            agentsById={agentsById}
            sort={sort}
            onSortChange={onSortChange}
            hasNextPage={filteredHook.hasNextPage}
            isFetchingNextPage={filteredHook.isFetchingNextPage}
            fetchNextPage={() => {
              void filteredHook.fetchNextPage();
            }}
            onOpenMtr={(measurementId, label) => setActiveMtr({ measurementId, label })}
          />
        )}

        {activeMtr ? (
          <MtrPanel
            campaign={campaign}
            measurementId={activeMtr.measurementId}
            label={activeMtr.label}
            onClose={() => setActiveMtr(null)}
          />
        ) : null}
      </div>
    </>
  );
}

interface CaptionInput {
  filteredTotal: number;
  unfilteredTotal: number;
  guardrailHidden: number;
  guardrailActive: boolean;
  isLoading: boolean;
}

function captionText({
  filteredTotal,
  unfilteredTotal,
  guardrailHidden,
  guardrailActive,
  isLoading,
}: CaptionInput): string {
  if (isLoading) return "Loading pair details…";
  const head = `Showing ${filteredTotal.toLocaleString()} of ${unfilteredTotal.toLocaleString()} rows for this candidate`;
  if (!guardrailActive || guardrailHidden === 0) {
    return head;
  }
  return `${head} · ${guardrailHidden.toLocaleString()} hidden by storage guardrails`;
}

function summarizeGuardrails(g: {
  min_improvement_ms: number | null;
  min_improvement_ratio: number | null;
  max_transit_rtt_ms: number | null;
  max_transit_stddev_ms: number | null;
}): string {
  const parts: string[] = [];
  if (g.min_improvement_ms !== null) parts.push(`Δ ≥ ${g.min_improvement_ms} ms`);
  if (g.min_improvement_ratio !== null) parts.push(`Δ ratio ≥ ${g.min_improvement_ratio}`);
  if (g.max_transit_rtt_ms !== null) parts.push(`transit RTT ≤ ${g.max_transit_rtt_ms} ms`);
  if (g.max_transit_stddev_ms !== null) parts.push(`transit σ ≤ ${g.max_transit_stddev_ms} ms`);
  return parts.join(" · ");
}
