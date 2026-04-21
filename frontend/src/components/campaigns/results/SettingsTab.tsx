import { useMemo, useState } from "react";
import type { Campaign, EvaluationMode } from "@/api/hooks/campaigns";
import { usePatchCampaign } from "@/api/hooks/campaigns";
import { useEvaluateCampaign, useEvaluation } from "@/api/hooks/evaluation";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import {
  extractCampaignErrorCode,
  isIllegalStateTransition,
  isNoBaselinePairs,
} from "@/lib/campaign";
import { clampKnob, KNOB_BOUNDS } from "@/lib/campaign-config";
import { useToastStore } from "@/stores/toast";

/**
 * Re-evaluate form. Persists the three evaluator-owned knobs via PATCH and
 * then fires POST /evaluate — /evaluate itself has no request body, so the
 * knobs must land on the campaign row first. Eligibility tracks the
 * backend's state gate (`completed | evaluated`).
 */

// ---------------------------------------------------------------------------
// Types + fixed copy (copy-paste from KnobPanel — the composer panel is too
// coupled to the full knob set to fork for this one sub-tab).
// ---------------------------------------------------------------------------

interface EvaluationKnobs {
  loss_threshold_pct: number;
  stddev_weight: number;
  evaluation_mode: EvaluationMode;
}

const DIVERSITY_HINT =
  "Evaluator qualifies a transit agent X when A → X → B beats the direct A → B path. Broader result set; surfaces every viable alternative route.";
const OPTIMIZATION_HINT =
  "Evaluator qualifies X only when A → X → B beats direct AND every existing mesh transit. Tighter result set; surfaces the genuinely best candidates.";

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export interface SettingsTabProps {
  campaign: Campaign;
}

export function SettingsTab({ campaign }: SettingsTabProps) {
  const evaluationQuery = useEvaluation(campaign.id);
  const patchMutation = usePatchCampaign();
  const evaluateMutation = useEvaluateCampaign();

  // Prefer the evaluation row's snapshot over the campaign row when present:
  // operators expect re-evaluate to start from "what produced the current
  // results", not from a subsequent PATCH that never ran. Fall back to the
  // campaign's own fields (NOT NULL at the DB level) before the first run.
  const initial = useMemo<EvaluationKnobs>(() => {
    const source = evaluationQuery.data ?? campaign;
    return {
      loss_threshold_pct: source.loss_threshold_pct,
      stddev_weight: source.stddev_weight,
      evaluation_mode: source.evaluation_mode,
    };
  }, [campaign, evaluationQuery.data]);

  // Keep the form locally editable, but rebase on a fresh seed when the
  // upstream changes (evaluation lands in cache, or the user navigates to
  // a different campaign). The signature comparison runs during render —
  // cheaper than a useEffect and React happily batches the state updates.
  const [form, setForm] = useState<EvaluationKnobs>(initial);
  const [seedSignature, setSeedSignature] = useState(() => JSON.stringify(initial));
  const nextSignature = JSON.stringify(initial);
  if (nextSignature !== seedSignature) {
    setForm(initial);
    setSeedSignature(nextSignature);
  }

  const isEligible = campaign.state === "completed" || campaign.state === "evaluated";
  const isPending = patchMutation.isPending || evaluateMutation.isPending;

  const handleNumber =
    (key: "loss_threshold_pct" | "stddev_weight") =>
    (event: React.ChangeEvent<HTMLInputElement>): void => {
      const raw = event.target.value;
      const parsed = raw === "" ? form[key] : Number(raw);
      setForm((prev) => ({ ...prev, [key]: clampKnob(key, parsed, prev[key]) }));
    };

  const handleMode = (next: string): void => {
    // Radix emits "" when the active item is toggled off — keep the
    // current mode rather than letting the knob go blank.
    if (next !== "diversity" && next !== "optimization") return;
    setForm((prev) => ({ ...prev, evaluation_mode: next }));
  };

  const toastError = (message: string): void => {
    useToastStore.getState().pushToast({ kind: "error", message });
  };

  const handleEvaluateError = (err: Error): void => {
    if (isNoBaselinePairs(err)) {
      toastError(
        "No baseline measurements available yet — add a pair or wait for in-flight measurements to settle.",
      );
      return;
    }
    if (isIllegalStateTransition(err)) {
      toastError("Campaign advanced before the re-evaluate request landed; refresh and retry.");
      return;
    }
    // `not_evaluated` is never raised by POST /evaluate itself — kept as
    // defence against spec drift via `extractCampaignErrorCode`.
    const code = extractCampaignErrorCode(err);
    toastError(code ? `Re-evaluate failed: ${code}` : `Re-evaluate failed: ${err.message}`);
  };

  const handlePatchError = (err: Error): void => {
    if (isIllegalStateTransition(err)) {
      toastError("Campaign advanced before the settings update landed; refresh and retry.");
      return;
    }
    toastError(`Saving settings failed: ${err.message}`);
  };

  const handleSubmit = (event: React.FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    if (!isEligible || isPending) return;
    patchMutation.mutate(
      {
        id: campaign.id,
        body: {
          loss_threshold_pct: form.loss_threshold_pct,
          stddev_weight: form.stddev_weight,
          evaluation_mode: form.evaluation_mode,
        },
      },
      {
        onSuccess: () => evaluateMutation.mutate(campaign.id, { onError: handleEvaluateError }),
        onError: handlePatchError,
      },
    );
  };

  return (
    <Card className="flex flex-col gap-4 p-6">
      <header className="flex flex-col gap-1">
        <h2 className="text-base font-semibold">Evaluation settings</h2>
        <p className="text-sm text-muted-foreground">
          Persist new threshold / mode values and re-run the evaluator against the existing
          baseline. Available once the campaign is Completed or Evaluated.
        </p>
      </header>

      <form onSubmit={handleSubmit} className="flex flex-col gap-4" aria-label="Re-evaluate form">
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="settings-loss-threshold">Loss threshold (%)</Label>
            <Input
              id="settings-loss-threshold"
              type="number"
              step="0.1"
              min={KNOB_BOUNDS.loss_threshold_pct.min}
              max={KNOB_BOUNDS.loss_threshold_pct.max}
              value={form.loss_threshold_pct}
              onChange={handleNumber("loss_threshold_pct")}
              disabled={!isEligible || isPending}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="settings-stddev-weight">Stddev weight</Label>
            <Input
              id="settings-stddev-weight"
              type="number"
              step="0.1"
              min={KNOB_BOUNDS.stddev_weight.min}
              max={KNOB_BOUNDS.stddev_weight.max}
              value={form.stddev_weight}
              onChange={handleNumber("stddev_weight")}
              disabled={!isEligible || isPending}
            />
          </div>
        </div>

        <div className="space-y-1">
          <Label id="settings-evaluation-mode-label">Evaluation mode</Label>
          <ToggleGroup
            type="single"
            value={form.evaluation_mode}
            onValueChange={handleMode}
            variant="outline"
            aria-labelledby="settings-evaluation-mode-label"
            aria-describedby="settings-evaluation-mode-hint"
            disabled={!isEligible || isPending}
          >
            {/*
             * No `aria-label` on the items — Radix derives the accessible
             * name from the visible child text when none is set, so the
             * announced labels match the visible "Diversity" / "Optimization"
             * copy exactly.
             */}
            <ToggleGroupItem value="diversity">Diversity</ToggleGroupItem>
            <ToggleGroupItem value="optimization">Optimization</ToggleGroupItem>
          </ToggleGroup>
          <p id="settings-evaluation-mode-hint" className="text-xs text-muted-foreground">
            {form.evaluation_mode === "diversity" ? DIVERSITY_HINT : OPTIMIZATION_HINT}
          </p>
        </div>

        <div className="flex items-center gap-3">
          <Button type="submit" disabled={!isEligible || isPending}>
            {isPending ? "Re-evaluating…" : "Re-evaluate"}
          </Button>
          {!isEligible ? (
            <p className="text-xs text-muted-foreground">
              Re-evaluate is available once the campaign is Completed or Evaluated.
            </p>
          ) : null}
        </div>
      </form>

      {evaluationQuery.data ? (
        <p className="text-xs text-muted-foreground">
          Last evaluated {evaluationQuery.data.evaluated_at} —{" "}
          {evaluationQuery.data.candidates_good} of {evaluationQuery.data.candidates_total}{" "}
          candidates qualified.
        </p>
      ) : null}
    </Card>
  );
}
