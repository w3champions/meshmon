/**
 * TripleDrawerBody — pair-detail body for diversity/optimization-mode candidates.
 *
 * Extracted from DrilldownDialog to keep the shell file focused on the
 * mode-dispatch and header layers. Renders the CandidatePairFilters toolbar,
 * caption math, and virtualized CandidatePairTable. See DrilldownDialog for
 * the full caption-math rationale.
 *
 * winning_x_position chips: `EvaluationPairDetailDto.winning_x_position` is
 * rendered per pair row inside `CandidatePairTable`. When `=== 1` the chip
 * reads "X first (A → X → Y → B)"; when `=== 2` it reads
 * "X second (A → Y → X → B)". The chip is absent when the field is null.
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
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import type { SortState } from "@/components/ui/sortable-header";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type Candidate = Evaluation["results"]["candidates"][number];

export interface TripleDrawerBodyProps {
  candidate: Candidate;
  campaign: Campaign;
  evaluation: Evaluation | null;
  unqualifiedReason: string | undefined;
  onClose: () => void;
}

const DEFAULT_QUERY: PairDetailsQuery = {
  sort: "improvement_ms",
  dir: "desc",
};

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function TripleDrawerBody({
  candidate,
  campaign,
  evaluation,
  onClose,
}: TripleDrawerBodyProps) {
  const [query, setQuery] = useState<PairDetailsQuery>(DEFAULT_QUERY);
  const [activeMtr, setActiveMtr] = useState<{
    measurementId: number;
    label: string;
  } | null>(null);

  const sort = useMemo<SortState<PairDetailSortCol>>(
    () => ({ col: query.sort, dir: query.dir }),
    [query.sort, query.dir],
  );

  const onSortChange = (col: PairDetailSortCol | null, dir: "asc" | "desc" | null) => {
    if (col === null || dir === null) {
      setQuery((prev) =>
        prev.sort === DEFAULT_QUERY.sort && prev.dir === DEFAULT_QUERY.dir
          ? { ...prev, dir: prev.dir === "asc" ? "desc" : "asc" }
          : { ...prev, ...DEFAULT_QUERY },
      );
    } else {
      setQuery((prev) => ({ ...prev, sort: col, dir }));
    }
    setActiveMtr(null);
  };

  const onFilterChange = (next: PairDetailsQuery) => {
    setQuery(next);
    setActiveMtr(null);
  };

  const unfilteredQuery = useMemo<PairDetailsQuery>(
    () => ({
      sort: query.sort,
      dir: query.dir,
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
              <strong>Not a candidate.</strong> The latest evaluation does not list this IP as a
              transit candidate. Re-run the evaluator if the candidate set has changed.
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
        ) : filteredHook.isLoading ? (
          <Card
            className="p-4 text-sm text-muted-foreground"
            role="status"
            aria-busy="true"
            data-testid="drilldown-loading"
          >
            Loading pair details…
          </Card>
        ) : filteredTotal === 0 && filterIsActive ? (
          <Card
            className="p-4 text-sm text-muted-foreground"
            role="status"
            data-testid="drilldown-empty-filters"
          >
            No rows match these filters. Clear filters or re-evaluate with looser guardrails.
          </Card>
        ) : filteredTotal === 0 && !filterIsActive ? (
          <Card
            className="p-4 text-sm text-muted-foreground"
            role="status"
            data-testid="drilldown-empty-guardrails"
          >
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

// ---------------------------------------------------------------------------
// Helpers (exported for reuse by DrilldownDialog)
// ---------------------------------------------------------------------------

interface CaptionInput {
  filteredTotal: number;
  unfilteredTotal: number;
  guardrailHidden: number;
  guardrailActive: boolean;
  isLoading: boolean;
}

export function captionText({
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

export function summarizeGuardrails(g: {
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
