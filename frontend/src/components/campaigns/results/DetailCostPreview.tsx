/**
 * Confirmation dialog shown before dispatching a Detail measurement sweep.
 *
 * Previews the expected `pairs_enqueued` count so the operator can gauge
 * the cost before firing a scope that may fan out across the full pair
 * list. Cost formulas (per spec §Task 18):
 *
 * - `scope = "all"`:    2 × (count of pairs in a settled state)
 * - `scope = "good_candidates"`: 4 × (de-duped qualifying pair-triples)
 * - `scope = "pair"`:   2 (a single pair → two pings + two MTR traces)
 *
 * On confirm the dialog fires `useTriggerDetail` with the pre-selected
 * scope. Error branches (`no_pairs_selected`, `no_evaluation`,
 * `illegal_state_transition`) funnel through dedicated toasts so the
 * operator gets a useful next-step instead of a raw error code.
 */

import { useEffect, useMemo, useState } from "react";
import type { Campaign } from "@/api/hooks/campaigns";
import type { DetailRequest, DetailScope, Evaluation } from "@/api/hooks/evaluation";
import { useTriggerDetail } from "@/api/hooks/evaluation";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  extractCampaignErrorCode,
  isIllegalStateTransition,
  isNoEvaluation,
  isNoPairsSelected,
} from "@/lib/campaign";
import { useToastStore } from "@/stores/toast";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/**
 * Scope surface used by the dialog. Matches the backend `DetailScope`
 * enum verbatim.
 */
export type DetailPreviewScope = DetailScope;

export interface DetailCostPreviewProps {
  /** Whether the dialog is mounted. */
  open: boolean;
  onOpenChange: (next: boolean) => void;
  /** Campaign the detail sweep targets. */
  campaign: Campaign;
  /** Scope the operator selected. Drives the preview label + mutation body. */
  scope: DetailPreviewScope;
  /**
   * Required when `scope === "pair"`. Ignored otherwise. Surfaced in the
   * dialog copy so the operator sees exactly which pair will be re-measured.
   */
  pair?: { source_agent_id: string; destination_ip: string };
  /**
   * Optional evaluation snapshot for the `good_candidates` preview cost —
   * the tab-level menu already reads it, so threading it down avoids a
   * second fetch and keeps the disabled-gate state in lockstep.
   */
  evaluation?: Evaluation | null;
}

// ---------------------------------------------------------------------------
// Cost math
// ---------------------------------------------------------------------------

/**
 * Settled-pair count — `succeeded` + `reused` count as "has data the
 * evaluator can act on". The preview only needs an approximation, so the
 * cost math stays deterministic off the campaign shell without querying
 * the full pair list.
 */
function countSettledPairs(campaign: Campaign): number {
  let total = 0;
  for (const [state, count] of campaign.pair_counts ?? []) {
    if (state === "succeeded" || state === "reused") total += count;
  }
  return total;
}

/**
 * De-duped qualifying-triple count off the evaluation's pair_details. Plan
 * §Task 18: `pairs_enqueued ≈ 4 × (qualifying-triple count)`; de-dup key
 * is `(source_agent_id, destination_ip, pair_kind-analogue)`. The
 * evaluation DTO does not carry a per-triple `kind`, so we dedupe on
 * `(source, destination_agent_id, candidate_destination_ip)` which is the
 * same triple identity the evaluator scored on.
 */
function countQualifyingTriples(evaluation: Evaluation): number {
  const seen = new Set<string>();
  for (const candidate of evaluation.results.candidates) {
    for (const pd of candidate.pair_details) {
      if (!pd.qualifies) continue;
      const key = `${pd.source_agent_id}|${pd.destination_agent_id}|${candidate.destination_ip}`;
      seen.add(key);
    }
  }
  return seen.size;
}

interface CostEstimate {
  pairs_enqueued: number;
  description: string;
}

export function computeCostEstimate(
  scope: DetailPreviewScope,
  campaign: Campaign,
  evaluation: Evaluation | null | undefined,
  pair: { source_agent_id: string; destination_ip: string } | undefined,
): CostEstimate {
  switch (scope) {
    case "all": {
      const settled = countSettledPairs(campaign);
      return {
        pairs_enqueued: 2 * settled,
        description: `Every settled pair (${settled.toLocaleString()}) is re-measured with a ping + MTR → ~2× the pair count.`,
      };
    }
    case "good_candidates": {
      // Distinguish "evaluation not loaded yet" from "evaluation loaded, zero
      // qualifying triples" — both look like `enqueue 0` numerically, but the
      // operator's next step differs (wait for the fetch vs re-run Evaluate).
      if (evaluation == null) {
        return {
          pairs_enqueued: 0,
          description: "Waiting for the evaluation to load before the cost can be estimated.",
        };
      }
      const triples = countQualifyingTriples(evaluation);
      return {
        pairs_enqueued: 4 * triples,
        description:
          triples > 0
            ? `${triples.toLocaleString()} qualifying pair-triples × 4 measurements (A→X ping+MTR, X→B ping+MTR).`
            : "No qualifying pairs in the current evaluation — re-run Evaluate if the thresholds changed.",
      };
    }
    case "pair": {
      const label = pair ? `${pair.source_agent_id} → ${pair.destination_ip}` : "selected pair";
      return {
        pairs_enqueued: 2,
        description: `Single pair (${label}) → one ping + one MTR trace (2 measurements).`,
      };
    }
  }
}

// ---------------------------------------------------------------------------
// Disabled-reason disambiguation
// ---------------------------------------------------------------------------

/**
 * The confirm button can be disabled for three distinct reasons. Collapsing
 * them into one "enqueue 0" label hides which follow-up action the operator
 * should take (wait for a fetch, re-run Evaluate, or widen the scope).
 */
export type ConfirmDisableReason = "inflight" | "loading_evaluation" | "no_pairs";

export function resolveDisableReason(
  scope: DetailPreviewScope,
  evaluation: Evaluation | null | undefined,
  pairsEnqueued: number,
  inflight: boolean,
): ConfirmDisableReason | null {
  if (inflight) return "inflight";
  if (scope === "good_candidates" && evaluation == null) return "loading_evaluation";
  if (pairsEnqueued === 0) return "no_pairs";
  return null;
}

// ---------------------------------------------------------------------------
// Copy helpers
// ---------------------------------------------------------------------------

function scopeTitle(scope: DetailPreviewScope): string {
  switch (scope) {
    case "all":
      return "Dispatch detail for every settled pair";
    case "good_candidates":
      return "Dispatch detail for good candidates";
    case "pair":
      return "Dispatch detail for this pair";
  }
}

function confirmLabel(disableReason: ConfirmDisableReason | null, pairsEnqueued: number): string {
  switch (disableReason) {
    case "inflight":
      return "Dispatching…";
    case "loading_evaluation":
      return "Loading evaluation…";
    case "no_pairs":
      return "No pairs to enqueue";
    case null:
      return `Confirm · enqueue ${pairsEnqueued}`;
  }
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function DetailCostPreview({
  open,
  onOpenChange,
  campaign,
  scope,
  pair,
  evaluation,
}: DetailCostPreviewProps) {
  const triggerDetail = useTriggerDetail();
  const [inflight, setInflight] = useState<boolean>(false);

  // If the dialog closes externally (escape, overlay click) while a request
  // is in flight, we don't abort the mutation — react-query owns that
  // lifecycle — but we DO reset the local spinner so the next open renders
  // a fresh state.
  useEffect(() => {
    if (!open) setInflight(false);
  }, [open]);

  const estimate = useMemo(
    () => computeCostEstimate(scope, campaign, evaluation, pair),
    [scope, campaign, evaluation, pair],
  );

  const disableReason = resolveDisableReason(scope, evaluation, estimate.pairs_enqueued, inflight);

  const handleConfirm = (): void => {
    if (inflight) return;
    const body: DetailRequest =
      scope === "pair"
        ? {
            scope: "pair",
            pair: pair ?? null,
          }
        : { scope };

    const { pushToast } = useToastStore.getState();
    setInflight(true);
    triggerDetail.mutate(
      { id: campaign.id, body },
      {
        onSuccess: (data) => {
          pushToast({
            kind: "success",
            message: `Enqueued ${data.pairs_enqueued.toLocaleString()} detail measurements.`,
          });
          setInflight(false);
          onOpenChange(false);
        },
        onError: (err) => {
          setInflight(false);
          if (isNoPairsSelected(err)) {
            pushToast({
              kind: "info",
              message: "Nothing left to re-measure — every eligible pair is already settled.",
            });
            return;
          }
          if (isNoEvaluation(err)) {
            pushToast({
              kind: "error",
              message: "Run Evaluate first — detail dispatch needs a persisted evaluation.",
            });
            return;
          }
          if (isIllegalStateTransition(err)) {
            pushToast({
              kind: "error",
              message:
                "Campaign is still running — wait for it to complete before dispatching detail.",
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
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent data-testid="detail-cost-preview">
        <DialogHeader>
          <DialogTitle>{scopeTitle(scope)}</DialogTitle>
          <DialogDescription>{estimate.description}</DialogDescription>
        </DialogHeader>

        <dl className="grid grid-cols-2 gap-3 rounded-md border p-3 text-sm">
          <div className="flex flex-col gap-0.5">
            <dt className="text-xs uppercase tracking-wide text-muted-foreground">
              Pairs enqueued
            </dt>
            <dd className="text-2xl font-semibold tabular-nums" data-testid="cost-preview-pairs">
              {estimate.pairs_enqueued.toLocaleString()}
            </dd>
          </div>
          <div className="flex flex-col gap-0.5">
            <dt className="text-xs uppercase tracking-wide text-muted-foreground">Scope</dt>
            <dd className="text-sm font-medium">{scope}</dd>
          </div>
        </dl>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={inflight}
          >
            Cancel
          </Button>
          <Button
            type="button"
            onClick={handleConfirm}
            disabled={disableReason !== null}
            data-testid="cost-preview-confirm"
          >
            {confirmLabel(disableReason, estimate.pairs_enqueued)}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
