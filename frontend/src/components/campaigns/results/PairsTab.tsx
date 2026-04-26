/**
 * Pairs tab — source-pair-centric view over `GET /api/campaigns/:id/pairs`.
 *
 * Complements the Candidates tab: where Candidates surfaces scored transit
 * options (A→X→B), this tab surfaces every configured baseline pair (A→B)
 * alongside its resolution state and dispatch history. The row action menu
 * is the operator's lever for per-pair re-measurement and detail dispatch —
 * Candidates' DrilldownDialog is read-only per pair and deliberately does
 * not carry these buttons.
 */

import { useCallback, useMemo } from "react";
import type { AgentSummary } from "@/api/hooks/agents";
import { useAgents } from "@/api/hooks/agents";
import type { Campaign } from "@/api/hooks/campaigns";
import { useCampaignPairs, useForcePair } from "@/api/hooks/campaigns";
import { useTriggerDetail } from "@/api/hooks/evaluation";
import { EdgePairsTab } from "@/components/campaigns/results/EdgePairsTab";
import {
  type PairRowAction,
  PairTable,
  type PairTableSort,
  usePairTableSort,
} from "@/components/campaigns/results/PairTable";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import {
  extractCampaignErrorCode,
  isIllegalStateTransition,
  isInvalidDestinationIp,
  isMissingPair,
  isNoPairsSelected,
} from "@/lib/campaign";
import { useToastStore } from "@/stores/toast";

const DEFAULT_SORT: PairTableSort = { col: "state", dir: "asc" };

/**
 * Request the backend's full pair cap. Without this the handler's default
 * page size (500) kicks in and campaigns above 500 baseline pairs silently
 * lose rows from the tab — the Pairs tab presents itself as the full
 * baseline view so that truncation is user-visibly wrong.
 */
const PAIRS_TAB_LIMIT = 5000;

export interface PairsTabProps {
  campaign: Campaign;
}

export function PairsTab({ campaign }: PairsTabProps) {
  // Edge-candidate mode delegates entirely to the flat (X, B) pivot.
  if (campaign.evaluation_mode === "edge_candidate") {
    return <EdgePairsTab campaign={campaign} />;
  }
  return <TriplePairsTab campaign={campaign} />;
}

/**
 * Pairs tab body for non-edge_candidate evaluation modes (optimization / diversity).
 * Renders the full baseline (A, B) pair table with force-remeasure and detail-dispatch
 * row actions.
 */
function TriplePairsTab({ campaign }: PairsTabProps) {
  const pairsQuery = useCampaignPairs(campaign.id, { limit: PAIRS_TAB_LIMIT });
  const agentsQuery = useAgents();
  const forcePairMutation = useForcePair();
  const triggerDetailMutation = useTriggerDetail();

  const agentsById = useMemo<Map<string, AgentSummary>>(() => {
    const map = new Map<string, AgentSummary>();
    for (const agent of agentsQuery.data ?? []) {
      map.set(agent.id, agent);
    }
    return map;
  }, [agentsQuery.data]);

  const [sort, setSort] = usePairTableSort(DEFAULT_SORT);

  const handleForcePair = useCallback(
    (pair: PairRowAction): void => {
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
    (pair: PairRowAction): void => {
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

  if (pairsQuery.isLoading) {
    return (
      <section
        data-testid="pairs-tab"
        role="status"
        aria-live="polite"
        className="flex flex-col gap-3"
      >
        <span className="sr-only">Loading pairs…</span>
        <Skeleton className="h-24 w-full" />
        <Skeleton className="h-64 w-full" />
      </section>
    );
  }

  if (pairsQuery.isError) {
    return (
      <section data-testid="pairs-tab" className="flex flex-col gap-3">
        <Card className="p-4 text-sm text-destructive" role="alert">
          Failed to load pairs: {pairsQuery.error?.message ?? "unknown error"}
        </Card>
      </section>
    );
  }

  const pairs = pairsQuery.data ?? [];

  return (
    <section data-testid="pairs-tab" className="flex flex-col gap-4">
      <PairTable
        pairs={pairs}
        protocol={campaign.protocol}
        agentsById={agentsById}
        onForcePair={handleForcePair}
        onTriggerPairDetail={handleTriggerPairDetail}
        sort={sort}
        onSortChange={setSort}
      />
    </section>
  );
}
