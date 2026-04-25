/**
 * Candidates tab — the default landing panel on `/campaigns/:id`.
 *
 * Composes the `CandidateTable` (summary KPIs + sortable table), the
 * `DrilldownDialog` (centered modal with paginated pair-detail rows
 * and per-pair MTR), per-row actions (force pair, dispatch detail for
 * a pair), and the tab-level `OverflowMenu` (Detail: all / Detail:
 * good candidates / Re-evaluate, with the cost-preview dialog wired
 * in for the Detail scopes).
 */

import { useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useMemo, useState } from "react";
import type { Campaign } from "@/api/hooks/campaigns";
import { useEvaluation } from "@/api/hooks/evaluation";
import { UnqualifiedReasons } from "@/components/campaigns/results/CandidatesTabParts";
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
}

const DEFAULT_SORT: CandidateTableSort = { col: "composite_score", dir: "desc" };

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function CandidatesTab({ campaign }: CandidatesTabProps) {
  const navigate = useNavigate();
  // `strict: false` keeps the hook usable under test harnesses that mount
  // the tab outside the registered route tree; the router's validator has
  // already coerced the shape so the cast here is safe.
  const search = useSearch({ strict: false }) as CampaignDetailSearch;

  const evaluationQuery = useEvaluation(campaign.id);

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

  const handleSelectCandidate = useCallback((ip: string): void => {
    setSelectedIp(ip);
  }, []);

  const handleCloseDialog = useCallback((): void => {
    setSelectedIp(null);
  }, []);

  // -------------------------------------------------------------------------
  // Empty + error branches
  // -------------------------------------------------------------------------

  if (evaluationQuery.isLoading) {
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

  if (evaluationQuery.isError) {
    return (
      <section data-testid="candidates-tab" className="flex flex-col gap-3">
        <Card className="p-4 text-sm text-destructive" role="alert">
          Failed to load evaluation: {evaluationQuery.error?.message ?? "unknown error"}
        </Card>
      </section>
    );
  }

  const evaluation = evaluationQuery.data;
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

      <CandidateTable
        evaluation={evaluation}
        selectedIp={selectedIp}
        onSelectCandidate={handleSelectCandidate}
        sort={sort}
        onSortChange={setSort}
      />

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
