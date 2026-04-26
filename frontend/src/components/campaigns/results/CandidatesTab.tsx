/**
 * Candidates tab — the default landing panel on `/campaigns/:id`.
 *
 * Composes the `CandidateTable` (summary KPIs + sortable table), the
 * `DrilldownDialog` (centered modal with paginated pair-detail rows
 * and per-pair MTR), and the tab-level `OverflowMenu` (Detail: all /
 * Detail: good candidates / Re-evaluate, with the cost-preview dialog
 * wired in for the Detail scopes).
 */

import { useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useMemo, useState } from "react";
import type { Campaign } from "@/api/hooks/campaigns";
import { type Evaluation, useEvaluation } from "@/api/hooks/evaluation";
import {
  type EdgeCandidateSort,
  EdgeCandidateTable,
  UnqualifiedReasons,
} from "@/components/campaigns/results/CandidatesTabParts";
import {
  type CandidateSortColumn,
  CandidateTable,
  type CandidateTableSort,
  type SortDirection,
} from "@/components/campaigns/results/CandidateTable";
import { DrilldownDialog } from "@/components/campaigns/results/DrilldownDialog";
import { OverflowMenu } from "@/components/campaigns/results/OverflowMenu";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import type { CampaignDetailSearch } from "@/router/index";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface CandidatesTabProps {
  campaign: Campaign;
  /**
   * Freshness-gated evaluation snapshot from the page shell. When the
   * campaign has been re-edited (a knob change flips state back to
   * `completed` and clears `evaluated_at`) or the snapshot's mode no
   * longer matches the campaign's mode, the page passes `null` so this
   * tab renders its evaluate-first placeholder instead of the stale
   * candidate rows. Mirrors the gate Heatmap / Pairs / Compare apply via
   * `freshEvaluation` in `CampaignDetail`.
   *
   * Optional for backward compatibility with stand-alone test mounts that
   * rely on the in-component `useEvaluation` query.
   */
  freshEvaluation?: Evaluation | null;
}

const DEFAULT_SORT: CandidateTableSort = { col: "composite_score", dir: "desc" };

const DEFAULT_EDGE_SORT: EdgeCandidateSort = { col: "coverage_weighted_ping_ms", dir: "asc" };

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function CandidatesTab({ campaign, freshEvaluation }: CandidatesTabProps) {
  const navigate = useNavigate();
  // `strict: false` keeps the hook usable under test harnesses that mount
  // the tab outside the registered route tree; the router's validator has
  // already coerced the shape so the cast here is safe.
  const search = useSearch({ strict: false }) as CampaignDetailSearch;

  const evaluationQuery = useEvaluation(campaign.id);
  // When the page passes an explicit `freshEvaluation` (always, in prod),
  // honour it as the source of truth for the rendered evaluation — that
  // prop already encodes the page-level freshness gate (state must be
  // `evaluated` AND snapshot's `evaluation_mode` must match the
  // campaign's current mode). Falling back to the query result keeps
  // stand-alone test mounts working without changes.
  const usePageEvaluation = freshEvaluation !== undefined;
  const renderedEvaluation = usePageEvaluation ? freshEvaluation : evaluationQuery.data;

  const [selectedIp, setSelectedIp] = useState<string | null>(null);

  const sort = useMemo<CandidateTableSort>(
    () => ({
      col: (search.cand_sort ?? DEFAULT_SORT.col) as CandidateSortColumn,
      dir: (search.cand_dir ?? DEFAULT_SORT.dir) as SortDirection,
    }),
    [search.cand_sort, search.cand_dir],
  );

  const setSort = useCallback(
    (next: CandidateTableSort): void => {
      // Spread the full search bag so sibling params (tab, raw_*) survive
      // the update — same pattern as the tab-shell's `onValueChange`.
      const navigateSearch = navigate as unknown as (opts: {
        search: CampaignDetailSearch;
        replace: boolean;
      }) => void;
      navigateSearch({
        search: { ...search, cand_sort: next.col, cand_dir: next.dir },
        replace: true,
      });
    },
    [navigate, search],
  );

  // Edge-candidate sort state (local only; edge sort not persisted to URL yet)
  const [edgeSort, setEdgeSort] = useState<EdgeCandidateSort>(DEFAULT_EDGE_SORT);

  const handleSelectCandidate = useCallback((ip: string): void => {
    setSelectedIp(ip);
  }, []);

  const handleCloseDialog = useCallback((): void => {
    setSelectedIp(null);
  }, []);

  // -------------------------------------------------------------------------
  // Empty + error branches
  // -------------------------------------------------------------------------
  //
  // Loading and error branches are driven by the in-component `useEvaluation`
  // query because the page-level shell does not surface those states through
  // `freshEvaluation`. When the page explicitly passes `null` to gate against
  // a stale or mode-mismatched snapshot, fall straight through to the
  // evaluate-first placeholder regardless of the query state.

  if (!usePageEvaluation && evaluationQuery.isLoading) {
    return (
      <section
        data-testid="candidates-tab"
        role="status"
        aria-live="polite"
        className="flex flex-col gap-3"
      >
        <span className="sr-only">Loading evaluation…</span>
        <Skeleton className="h-24 w-full" />
        <Skeleton className="h-64 w-full" />
      </section>
    );
  }

  if (!usePageEvaluation && evaluationQuery.isError) {
    return (
      <section data-testid="candidates-tab" className="flex flex-col gap-3">
        <Card className="p-4 text-sm text-destructive" role="alert">
          Failed to load evaluation: {evaluationQuery.error?.message ?? "unknown error"}
        </Card>
      </section>
    );
  }

  const evaluation = renderedEvaluation;
  if (!evaluation) {
    return (
      <section data-testid="candidates-tab" className="flex flex-col gap-3">
        <Card className="flex flex-col items-start gap-3 p-6">
          <div>
            <h2 className="text-base font-semibold">No evaluation yet</h2>
            <p className="text-sm text-muted-foreground">
              Run the evaluator from the Settings tab to score transit candidates against the
              baseline pairs.
            </p>
          </div>
          <Button
            onClick={() => {
              const navigateSearch = navigate as unknown as (opts: {
                search: CampaignDetailSearch;
                replace: boolean;
              }) => void;
              navigateSearch({
                search: { ...search, tab: "settings" },
                replace: true,
              });
            }}
          >
            Open settings tab
          </Button>
        </Card>
      </section>
    );
  }

  // Selected candidate (for the drawer) + matching unqualified reason.
  const selectedCandidate =
    selectedIp === null
      ? null
      : (evaluation.results.candidates.find((c) => c.destination_ip === selectedIp) ?? null);
  const unqualifiedReason =
    selectedIp !== null ? evaluation.results.unqualified_reasons?.[selectedIp] : undefined;

  return (
    <section data-testid="candidates-tab" className="flex flex-col gap-4">
      {/* Tab-level overflow menu — Detail: all / Detail: good candidates /
          Re-evaluate. Good-candidates is gated strictly on
          `campaign.state === "evaluated"`; see OverflowMenu's docstring for
          why a stale evaluation on a completed campaign must NOT re-enable
          the action. */}
      <div className="flex justify-end">
        <OverflowMenu campaign={campaign} evaluation={evaluation} />
      </div>

      {evaluation.evaluation_mode === "edge_candidate" ? (
        <EdgeCandidateTable
          evaluation={evaluation}
          selectedIp={selectedIp}
          onSelectCandidate={handleSelectCandidate}
          sort={edgeSort}
          onSortChange={setEdgeSort}
        />
      ) : (
        <CandidateTable
          evaluation={evaluation}
          selectedIp={selectedIp}
          onSelectCandidate={handleSelectCandidate}
          sort={sort}
          onSortChange={setSort}
        />
      )}

      {Object.keys(evaluation.results.unqualified_reasons ?? {}).length > 0 &&
      selectedIp === null ? (
        <UnqualifiedReasons reasons={evaluation.results.unqualified_reasons ?? {}} />
      ) : null}

      <DrilldownDialog
        candidate={selectedCandidate}
        campaign={campaign}
        evaluation={evaluation}
        onClose={handleCloseDialog}
        unqualifiedReason={unqualifiedReason}
      />
    </section>
  );
}
