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
  extractCampaignErrorDetail,
  isIllegalStateTransition,
  isNoBaselinePairs,
  isVmUpstream,
} from "@/lib/campaign";
import {
  clampKnob,
  KNOB_BOUNDS,
  nullableKnobInputValue,
  parseNullableKnob,
  ratioToPercentInput,
} from "@/lib/campaign-config";
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
  /** Wire-format ratio in `[0, 1]`; the form renders it as percent. */
  loss_threshold_ratio: number;
  stddev_weight: number;
  evaluation_mode: EvaluationMode;
  /**
   * Guardrail knobs. `null` means "gate disabled" — the input is empty
   * and the submit body sends `null`. Note: the PATCH backend uses
   * `COALESCE($n, col)` for these columns, so a `null` payload preserves
   * whatever value the column already holds rather than clearing it back
   * to NULL — clearing the input only resets the local form state.
   */
  max_transit_rtt_ms: number | null;
  max_transit_stddev_ms: number | null;
  min_improvement_ms: number | null;
  min_improvement_ratio: number | null;
}

/**
 * True when the evaluation snapshot has at least one guardrail knob set.
 * Used to gate the "guardrails dropped every candidate" footgun warning —
 * the warning is meaningful only when an active gate could explain the
 * empty result set.
 */
function hasAnyGuardrailSet(snapshot: {
  max_transit_rtt_ms?: number | null;
  max_transit_stddev_ms?: number | null;
  min_improvement_ms?: number | null;
  min_improvement_ratio?: number | null;
}): boolean {
  return (
    snapshot.max_transit_rtt_ms != null ||
    snapshot.max_transit_stddev_ms != null ||
    snapshot.min_improvement_ms != null ||
    snapshot.min_improvement_ratio != null
  );
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
      loss_threshold_ratio: source.loss_threshold_ratio,
      stddev_weight: source.stddev_weight,
      evaluation_mode: source.evaluation_mode,
      // `?? null` collapses `undefined` (campaigns created before the
      // guardrail columns shipped) to the canonical "off" sentinel so
      // the form never holds an out-of-band value.
      max_transit_rtt_ms: source.max_transit_rtt_ms ?? null,
      max_transit_stddev_ms: source.max_transit_stddev_ms ?? null,
      min_improvement_ms: source.min_improvement_ms ?? null,
      min_improvement_ratio: source.min_improvement_ratio ?? null,
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
    (key: "stddev_weight") =>
    (event: React.ChangeEvent<HTMLInputElement>): void => {
      const raw = event.target.value;
      const parsed = raw === "" ? form[key] : Number(raw);
      setForm((prev) => ({ ...prev, [key]: clampKnob(key, parsed, prev[key]) }));
    };

  // Guardrail handler. Empty input collapses to `null` in the local
  // form state. On submit, `null` flows through to the wire — the
  // backend's `COALESCE` semantics mean the existing column value is
  // preserved rather than cleared, which matches the existing knob
  // convention.
  type NullableGuardrailKey =
    | "max_transit_rtt_ms"
    | "max_transit_stddev_ms"
    | "min_improvement_ms"
    | "min_improvement_ratio";

  const handleNullable =
    (key: NullableGuardrailKey) =>
    (event: React.ChangeEvent<HTMLInputElement>): void => {
      setForm((prev) => ({
        ...prev,
        [key]: parseNullableKnob(key, event.target.value, prev[key]),
      }));
    };

  // Loss threshold: input is percent-facing for UX, but the wire value is a
  // ratio — convert at the form boundary so the submitted body stays in
  // ratio units while the operator keeps typing percent-style numbers.
  const handleLossThresholdPct = (event: React.ChangeEvent<HTMLInputElement>): void => {
    const raw = event.target.value;
    if (raw === "") return;
    const percent = Number(raw);
    if (!Number.isFinite(percent)) return;
    const ratio = percent / 100;
    setForm((prev) => ({
      ...prev,
      loss_threshold_ratio: clampKnob("loss_threshold_ratio", ratio, prev.loss_threshold_ratio),
    }));
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
    if (isVmUpstream(err)) {
      const detail = extractCampaignErrorDetail(err);
      toastError(
        detail
          ? `VictoriaMetrics couldn't be reached for baseline data (${detail}). Check service config and retry.`
          : "VictoriaMetrics couldn't be reached for baseline data. Check service config and retry.",
      );
      return;
    }
    if (isNoBaselinePairs(err)) {
      toastError(
        "No agent-to-agent baseline measurements exist for this campaign yet. Add a pair or wait for in-flight measurements to settle, then retry.",
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
          loss_threshold_ratio: form.loss_threshold_ratio,
          stddev_weight: form.stddev_weight,
          evaluation_mode: form.evaluation_mode,
          // Guardrails ride along on every PATCH so a re-evaluate runs
          // against the operator's current intent. `null` is the wire
          // representation of "leave the column untouched" via the
          // backend's `COALESCE($n, col)` PATCH semantics.
          max_transit_rtt_ms: form.max_transit_rtt_ms,
          max_transit_stddev_ms: form.max_transit_stddev_ms,
          min_improvement_ms: form.min_improvement_ms,
          min_improvement_ratio: form.min_improvement_ratio,
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
          Persist new threshold / mode values and re-run the evaluator against this campaign's
          measurements. Available once the campaign is Completed or Evaluated.
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
              min={KNOB_BOUNDS.loss_threshold_ratio.min * 100}
              max={KNOB_BOUNDS.loss_threshold_ratio.max * 100}
              value={ratioToPercentInput(form.loss_threshold_ratio)}
              onChange={handleLossThresholdPct}
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

        {/*
         * Guardrail knobs. Optional — empty input clears the local form
         * state. NOTE: PATCH semantics on the backend use `COALESCE($n,
         * col)` for these columns, mirroring `loss_threshold_ratio` and
         * `stddev_weight`; clearing an input here does NOT push the
         * column back to NULL on the server. To disable a guardrail
         * after it has been set, clone the campaign and start fresh.
         */}
        <div className="space-y-1">
          <Label className="text-sm font-semibold">Evaluation guardrails (optional)</Label>
          <p id="settings-guardrails-hint" className="text-xs text-muted-foreground">
            Eligibility caps prune transit candidates before scoring; storage floors gate which
            per-pair rows are persisted (combined under OR semantics). Clearing an input only resets
            the local form — operators cannot disable a guardrail once set; clone the campaign to
            start fresh.
          </p>
        </div>
        <div className="grid gap-3 sm:grid-cols-2" aria-describedby="settings-guardrails-hint">
          <div className="space-y-1">
            <Label htmlFor="settings-max-transit-rtt-ms">Max transit RTT (ms)</Label>
            <Input
              id="settings-max-transit-rtt-ms"
              type="number"
              min={KNOB_BOUNDS.max_transit_rtt_ms.min}
              max={KNOB_BOUNDS.max_transit_rtt_ms.max}
              value={nullableKnobInputValue(form.max_transit_rtt_ms)}
              placeholder="e.g. 200"
              onChange={handleNullable("max_transit_rtt_ms")}
              disabled={!isEligible || isPending}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="settings-max-transit-stddev-ms">Max transit RTT stddev (ms)</Label>
            <Input
              id="settings-max-transit-stddev-ms"
              type="number"
              min={KNOB_BOUNDS.max_transit_stddev_ms.min}
              max={KNOB_BOUNDS.max_transit_stddev_ms.max}
              value={nullableKnobInputValue(form.max_transit_stddev_ms)}
              placeholder="e.g. 50"
              onChange={handleNullable("max_transit_stddev_ms")}
              disabled={!isEligible || isPending}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="settings-min-improvement-ms">Min improvement (ms)</Label>
            <Input
              id="settings-min-improvement-ms"
              type="number"
              step="0.1"
              min={KNOB_BOUNDS.min_improvement_ms.min}
              max={KNOB_BOUNDS.min_improvement_ms.max}
              value={nullableKnobInputValue(form.min_improvement_ms)}
              placeholder="e.g. 5 (negative values allowed)"
              onChange={handleNullable("min_improvement_ms")}
              disabled={!isEligible || isPending}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="settings-min-improvement-ratio">Min improvement ratio</Label>
            <Input
              id="settings-min-improvement-ratio"
              type="number"
              step="0.01"
              min={KNOB_BOUNDS.min_improvement_ratio.min}
              max={KNOB_BOUNDS.min_improvement_ratio.max}
              value={nullableKnobInputValue(form.min_improvement_ratio)}
              placeholder="e.g. 0.1 (10%)"
              onChange={handleNullable("min_improvement_ratio")}
              disabled={!isEligible || isPending}
            />
          </div>
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

      {/*
       * Operator-footgun warning. A tight guardrail set against a sparse
       * baseline can drop every candidate; the operator sees "0 of 0"
       * above and may assume the campaign produced no useful data. The
       * warning fires only when at least one guardrail is set on the
       * snapshot — an empty result set with no guardrails is a different
       * shape (likely no baseline pairs).
       */}
      {evaluationQuery.data &&
      evaluationQuery.data.candidates_total === 0 &&
      hasAnyGuardrailSet(evaluationQuery.data) ? (
        <p className="text-xs text-amber-600 dark:text-amber-400" role="status">
          The active guardrails dropped every candidate. Loosen one or more knobs and re-evaluate to
          see results.
        </p>
      ) : null}
    </Card>
  );
}
