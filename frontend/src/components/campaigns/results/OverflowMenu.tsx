/**
 * Tab-level overflow menu for the Candidates tab.
 *
 * Replaces the Batch 4 placeholder with three real operator actions:
 *
 * 1. **Detail: all** — re-measures every settled pair. Opens the
 *    {@link DetailCostPreview} dialog with `scope="all"`.
 * 2. **Detail: good candidates only** — re-measures pairs the evaluator
 *    flagged as qualifying. Opens the dialog with `scope="good_candidates"`.
 *    Disabled unless `campaign.state === "evaluated"` — a stale evaluation
 *    on a `completed` campaign must NOT re-enable this, matching the
 *    backend gate. The disabled-tooltip steers the operator to Evaluate
 *    first.
 * 3. **Re-evaluate** — idempotent; does NOT open the cost dialog since it
 *    only re-scores existing pairs (no new measurements). Errors bubble
 *    as toasts through the same matcher set used by `SettingsTab`.
 *
 * The `data-testid` hooks (`overflow-detail-all`, `overflow-detail-good`,
 * `overflow-re-evaluate`, `candidates-overflow-trigger`) are load-bearing —
 * they back the Batch 4 overflow-menu gating tests — keep them stable.
 */

import { useState } from "react";
import type { Campaign } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";
import { useEvaluateCampaign } from "@/api/hooks/evaluation";
import { DetailCostPreview } from "@/components/campaigns/results/DetailCostPreview";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  extractCampaignErrorCode,
  extractCampaignErrorDetail,
  isIllegalStateTransition,
  isNoBaselinePairs,
  isVmNotConfigured,
  isVmUpstream,
} from "@/lib/campaign";
import { useToastStore } from "@/stores/toast";

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface OverflowMenuProps {
  campaign: Campaign;
  /**
   * Current evaluation row — threaded in so the cost-preview dialog can
   * compute the qualifying-triple count without issuing a second fetch.
   * `null` when the campaign has never been evaluated; that's fine because
   * the "good candidates" item is disabled in that state anyway.
   */
  evaluation?: Evaluation | null;
}

type DialogScope = "all" | "good_candidates";

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function OverflowMenu({ campaign, evaluation }: OverflowMenuProps) {
  const evaluateMutation = useEvaluateCampaign();

  const goodCandidatesEnabled = campaign.state === "evaluated";

  const [dialogScope, setDialogScope] = useState<DialogScope | null>(null);

  const handleReEvaluate = (): void => {
    const { pushToast } = useToastStore.getState();
    evaluateMutation.mutate(campaign.id, {
      onSuccess: () => {
        pushToast({ kind: "success", message: "Re-evaluation complete." });
      },
      onError: (err) => {
        if (isVmNotConfigured(err)) {
          pushToast({
            kind: "error",
            message:
              "VictoriaMetrics isn't configured for this deployment — set `[upstream.vm_url]` and retry.",
          });
          return;
        }
        if (isVmUpstream(err)) {
          const detail = extractCampaignErrorDetail(err);
          pushToast({
            kind: "error",
            message: detail
              ? `VictoriaMetrics is unreachable: ${detail}`
              : "VictoriaMetrics is unreachable.",
          });
          return;
        }
        if (isNoBaselinePairs(err)) {
          pushToast({
            kind: "error",
            message:
              "No agent-to-agent baselines from VictoriaMetrics in the last 15 minutes — wait for continuous-mesh data to accrue, or verify agents are online.",
          });
          return;
        }
        if (isIllegalStateTransition(err)) {
          pushToast({
            kind: "error",
            message: "Campaign advanced before the re-evaluate request landed — refresh and retry.",
          });
          return;
        }
        const code = extractCampaignErrorCode(err);
        pushToast({
          kind: "error",
          message: code ? `Re-evaluate failed: ${code}` : `Re-evaluate failed: ${err.message}`,
        });
      },
    });
  };

  return (
    <>
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
            data-testid="overflow-detail-all"
            onSelect={(event) => {
              // `onSelect` closes the menu automatically; prevent default so
              // the dialog can open in the same tick without a layout churn.
              event.preventDefault();
              setDialogScope("all");
            }}
          >
            Detail: all
          </DropdownMenuItem>
          <DropdownMenuItem
            data-testid="overflow-detail-good"
            disabled={!goodCandidatesEnabled}
            aria-disabled={!goodCandidatesEnabled}
            title={
              goodCandidatesEnabled ? undefined : "Run Evaluate first to act on good candidates."
            }
            onSelect={(event) => {
              if (!goodCandidatesEnabled) return;
              event.preventDefault();
              setDialogScope("good_candidates");
            }}
          >
            Detail: good candidates only
          </DropdownMenuItem>
          <DropdownMenuItem
            data-testid="overflow-re-evaluate"
            disabled={evaluateMutation.isPending}
            onSelect={(event) => {
              event.preventDefault();
              handleReEvaluate();
            }}
          >
            {evaluateMutation.isPending ? "Re-evaluating…" : "Re-evaluate"}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      {dialogScope ? (
        <DetailCostPreview
          open
          onOpenChange={(next) => {
            if (!next) setDialogScope(null);
          }}
          campaign={campaign}
          scope={dialogScope}
          evaluation={evaluation ?? null}
        />
      ) : null}
    </>
  );
}
