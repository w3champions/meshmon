/**
 * Candidates tab — the default landing panel on `/campaigns/:id`.
 *
 * Composes the Task 13 `CandidateTable` (summary KPIs + sortable table),
 * the Task 14 `DrilldownDrawer` (right-side sheet with per-pair MTR), and
 * per-row actions (force pair, dispatch detail for a pair). The tab-level
 * overflow menu (Detail: all / Detail: good candidates / Re-evaluate) is
 * wired in Task 18 (Batch 5) via a dedicated `OverflowMenu` component —
 * a placeholder is rendered here so the Batch 5 drop-in has a clean mount
 * point.
 */

import { useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useMemo, useState } from "react";
import type { Campaign } from "@/api/hooks/campaigns";
import { useForcePair } from "@/api/hooks/campaigns";
import { useEvaluation, useTriggerDetail } from "@/api/hooks/evaluation";
import {
  type CandidateSortColumn,
  CandidateTable,
  type CandidateTableSort,
  type SortDirection,
} from "@/components/campaigns/results/CandidateTable";
import { DrilldownDrawer } from "@/components/campaigns/results/DrilldownDrawer";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import {
  extractCampaignErrorCode,
  isIllegalStateTransition,
  isInvalidDestinationIp,
  isMissingPair,
  isNoPairsSelected,
} from "@/lib/campaign";
import type { CampaignDetailSearch } from "@/router/index";
import { useToastStore } from "@/stores/toast";

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
  const forcePairMutation = useForcePair();
  const triggerDetailMutation = useTriggerDetail();

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

  const handleCloseDrawer = useCallback((): void => {
    setSelectedIp(null);
  }, []);

  // -------------------------------------------------------------------------
  // Row actions
  // -------------------------------------------------------------------------

  const handleForcePair = useCallback(
    (pair: { source_agent_id: string; destination_ip: string }): void => {
      const { pushToast } = useToastStore.getState();
      forcePairMutation.mutate(
        { id: campaign.id, body: pair },
        {
          onSuccess: () => {
            pushToast({
              kind: "success",
              message: `Queued force re-measure for ${pair.destination_ip}.`,
            });
          },
          onError: (err) => {
            if (isIllegalStateTransition(err)) {
              pushToast({
                kind: "error",
                message: "Can't force pair — campaign advanced before the request landed.",
              });
              return;
            }
            if (isMissingPair(err)) {
              pushToast({
                kind: "error",
                message: `Pair ${pair.destination_ip} no longer exists on this campaign.`,
              });
              return;
            }
            const code = extractCampaignErrorCode(err);
            pushToast({
              kind: "error",
              message: code ? `Force pair failed: ${code}` : `Force pair failed: ${err.message}`,
            });
          },
        },
      );
    },
    [forcePairMutation, campaign.id],
  );

  const handleTriggerPairDetail = useCallback(
    (pair: { source_agent_id: string; destination_ip: string }): void => {
      const { pushToast } = useToastStore.getState();
      triggerDetailMutation.mutate(
        {
          id: campaign.id,
          body: {
            scope: "pair",
            pair: {
              source_agent_id: pair.source_agent_id,
              destination_ip: pair.destination_ip,
            },
          },
        },
        {
          onSuccess: (data) => {
            pushToast({
              kind: "success",
              message: `Enqueued ${data.pairs_enqueued} detail measurements for ${pair.destination_ip}.`,
            });
          },
          onError: (err) => {
            if (isInvalidDestinationIp(err)) {
              pushToast({
                kind: "error",
                message: `Can't dispatch detail — destination IP ${pair.destination_ip} is malformed.`,
              });
              return;
            }
            if (isMissingPair(err)) {
              pushToast({
                kind: "error",
                message: `Pair ${pair.destination_ip} no longer exists on this campaign.`,
              });
              return;
            }
            if (isNoPairsSelected(err)) {
              pushToast({
                kind: "error",
                message: "No pairs qualified for detail dispatch — nothing to remeasure.",
              });
              return;
            }
            const code = extractCampaignErrorCode(err);
            pushToast({
              kind: "error",
              message: code
                ? `Detail dispatch failed: ${code}`
                : `Detail dispatch failed: ${err.message}`,
            });
          },
        },
      );
    },
    [triggerDetailMutation, campaign.id],
  );

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
      {/* Tab-level overflow menu mount point. The real dropdown lands in
          Task 18 (Batch 5) alongside the DetailCostPreview dialog. */}
      <div className="flex justify-end">
        <TabOverflowPlaceholder campaign={campaign} />
      </div>

      <CandidateTable
        evaluation={evaluation}
        selectedIp={selectedIp}
        onSelectCandidate={handleSelectCandidate}
        sort={sort}
        onSortChange={setSort}
        renderRowActions={(candidate) => (
          <RowActionMenu
            candidate={candidate}
            onForcePair={handleForcePair}
            onTriggerPairDetail={handleTriggerPairDetail}
          />
        )}
      />

      {Object.keys(evaluation.results.unqualified_reasons ?? {}).length > 0 &&
      selectedIp === null ? (
        <UnqualifiedReasons reasons={evaluation.results.unqualified_reasons ?? {}} />
      ) : null}

      <DrilldownDrawer
        candidate={selectedCandidate}
        campaign={campaign}
        onClose={handleCloseDrawer}
        unqualifiedReason={unqualifiedReason}
      />
    </section>
  );
}

// ---------------------------------------------------------------------------
// Row action menu
// ---------------------------------------------------------------------------

interface RowActionMenuProps {
  candidate: { destination_ip: string; pair_details: { source_agent_id: string }[] };
  onForcePair: (pair: { source_agent_id: string; destination_ip: string }) => void;
  onTriggerPairDetail: (pair: { source_agent_id: string; destination_ip: string }) => void;
}

function RowActionMenu({ candidate, onForcePair, onTriggerPairDetail }: RowActionMenuProps) {
  // The "pair" the row actions operate on uses the first scored pair's
  // source agent — the full (source, destination) addressing lives in
  // `pair_details`, and both `force_pair` and detail-scope-pair hit the
  // server-side pair row by `(source, destination_ip)`. When a candidate
  // has multiple pairs, the drawer is the operator's lever for per-pair
  // dispatch; the row-level shortcut targets the highest-scoring pair.
  const firstPair = candidate.pair_details[0];
  if (!firstPair) return null;

  const pair = {
    source_agent_id: firstPair.source_agent_id,
    destination_ip: candidate.destination_ip,
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          aria-label={`Actions for ${candidate.destination_ip}`}
        >
          ⋯
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem onClick={() => onForcePair(pair)}>Force re-measure pair</DropdownMenuItem>
        <DropdownMenuItem onClick={() => onTriggerPairDetail(pair)}>
          Dispatch detail for this pair
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

// ---------------------------------------------------------------------------
// Unqualified-reasons section
// ---------------------------------------------------------------------------

interface UnqualifiedReasonsProps {
  reasons: Record<string, string>;
}

function UnqualifiedReasons({ reasons }: UnqualifiedReasonsProps) {
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
// Tab overflow placeholder
// ---------------------------------------------------------------------------

interface TabOverflowPlaceholderProps {
  campaign: Campaign;
}

/**
 * Placeholder trigger for the Task 18 `OverflowMenu`. Task 15 gates the
 * "Detail: good candidates only" item strictly on `campaign.state ===
 * "evaluated"` (a stale evaluation on a `completed` campaign should NOT
 * re-enable it). The placeholder exposes the gate state via a disabled
 * affordance so integration tests can assert on it today; Batch 5 swaps
 * the placeholder for the real dropdown with confirmation dialogs.
 */
function TabOverflowPlaceholder({ campaign }: TabOverflowPlaceholderProps) {
  const goodCandidatesEnabled = campaign.state === "evaluated";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-label="Candidates tab actions"
          data-testid="candidates-overflow-trigger"
        >
          Actions
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem
          disabled
          data-testid="overflow-detail-all"
          aria-describedby="overflow-detail-all-hint"
        >
          Detail: all (Batch 5)
        </DropdownMenuItem>
        <DropdownMenuItem
          disabled={!goodCandidatesEnabled}
          data-testid="overflow-detail-good"
          aria-disabled={!goodCandidatesEnabled}
        >
          Detail: good candidates only (Batch 5)
        </DropdownMenuItem>
        <DropdownMenuItem disabled data-testid="overflow-re-evaluate">
          Re-evaluate (Batch 5)
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
