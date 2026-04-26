import { useMemo, useState } from "react";
import type { Campaign, EvaluationMode } from "@/api/hooks/campaigns";
import { usePatchCampaign } from "@/api/hooks/campaigns";
import { useEvaluateCampaign, useEvaluation } from "@/api/hooks/evaluation";
import { Badge } from "@/components/ui/badge";
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
// Types + fixed copy
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
  /** edge_candidate-only. `null` means not set (required in edge_candidate). */
  useful_latency_ms: number | null;
  /** 0–2 in edge_candidate; 1–2 in diversity/optimization. */
  max_hops: number;
  /** Look-back window for VictoriaMetrics queries. Range [1, 1440]. */
  vm_lookback_minutes: number;
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
const EDGE_CANDIDATE_HINT =
  "Evaluator scores X candidates by their direct + transitive (X → A → B) connectivity to a fixed set of mesh agents. Use to evaluate new edge-node locations.";

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

  // Prefer the evaluation row's snapshot over the campaign row when it is
  // *fresh* — i.e. the campaign is in `evaluated` state and the snapshot's
  // mode matches the current campaign mode. A knob-change PATCH dismisses
  // the evaluation by flipping `state` back to `completed` while the
  // historical `campaign_evaluations` row stays around for the read-side
  // history view. Without the freshness gate the form would seed from that
  // stale row and silently push the previous knob values back over the
  // operator's PATCH on the next submit.
  //
  // This mirrors `hasFreshEvaluation` in `CampaignDetail.tsx` so the
  // evaluation-derived tabs and the SettingsTab form share one notion
  // of "current snapshot vs. historical row".
  const isFreshEvaluation =
    campaign.state === "evaluated" &&
    evaluationQuery.data?.evaluation_mode === campaign.evaluation_mode;
  const snapshotForForm = isFreshEvaluation ? evaluationQuery.data : null;

  const initial = useMemo<EvaluationKnobs>(() => {
    // Fall back to the campaign row whenever the snapshot is stale (or
    // absent) so a post-dismissal reopen reflects the operator's most
    // recent PATCH, not the historical evaluation that triggered it.
    const source = snapshotForForm ?? campaign;
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
      // New edge_candidate knobs: `?? null` / `?? default` for legacy snapshots.
      useful_latency_ms: source.useful_latency_ms ?? null,
      max_hops: source.max_hops ?? KNOB_BOUNDS.max_hops.max,
      vm_lookback_minutes: source.vm_lookback_minutes ?? 15,
    };
  }, [campaign, snapshotForForm]);

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
  const isEdgeCandidate = form.evaluation_mode === "edge_candidate";

  const handleNumber =
    (key: "stddev_weight" | "vm_lookback_minutes") =>
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
    | "min_improvement_ratio"
    | "useful_latency_ms";

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
    if (next !== "diversity" && next !== "optimization" && next !== "edge_candidate") return;
    const mode = next as EvaluationMode;
    // When leaving edge_candidate, clamp max_hops to [1, 2] since 0 is invalid for other modes.
    const hopsFix: Partial<EvaluationKnobs> = mode !== "edge_candidate" && form.max_hops === 0 ? { max_hops: 1 } : {};
    setForm((prev) => ({ ...prev, evaluation_mode: mode, ...hopsFix }));
  };

  const evaluationModeHint =
    form.evaluation_mode === "diversity"
      ? DIVERSITY_HINT
      : form.evaluation_mode === "edge_candidate"
        ? EDGE_CANDIDATE_HINT
        : OPTIMIZATION_HINT;

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
    if (isEdgeCandidate && form.useful_latency_ms === null) return;
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
          max_hops: form.max_hops,
          vm_lookback_minutes: form.vm_lookback_minutes,
          // useful_latency_ms is required in edge_candidate; omitted for
          // diversity/optimization when null so the backend doesn't receive
          // a null value for a non-edge mode.
          ...(isEdgeCandidate ? { useful_latency_ms: form.useful_latency_ms } : {}),
        },
      },
      {
        onSuccess: () => evaluateMutation.mutate(campaign.id, { onError: handleEvaluateError }),
        onError: handlePatchError,
      },
    );
  };

  // Submit-disabled: ineligible campaign, pending request, or edge_candidate with no useful_latency_ms.
  const isSubmitDisabled =
    !isEligible || isPending || (isEdgeCandidate && form.useful_latency_ms === null);

  // The CampaignDto doesn't expose source_agent_ids in the TypeScript type;
  // use a widened cast matching the pattern in CampaignComposer.tsx.
  const sourceAgentIds =
    (campaign as unknown as { source_agent_ids?: string[] }).source_agent_ids ?? [];

  const snapshot = evaluationQuery.data;
  const isLegacySnapshot = snapshot != null && snapshot.max_hops == null;
  // Reads the *committed* snapshot mode, not the pending form selection — the banner describes the persisted edge_candidate evaluation, not a pending mode switch.
  const showSingleSourceBanner =
    snapshot != null &&
    snapshot.evaluation_mode === "edge_candidate" &&
    sourceAgentIds.length === 1;

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
        {/* Mode selector at top per spec §6.2 */}
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
             * announced labels match the visible copy exactly.
             */}
            <ToggleGroupItem value="diversity">Diversity</ToggleGroupItem>
            <ToggleGroupItem value="optimization">Optimization</ToggleGroupItem>
            <ToggleGroupItem value="edge_candidate">Edge candidate</ToggleGroupItem>
          </ToggleGroup>
          <p id="settings-evaluation-mode-hint" className="text-xs text-muted-foreground">
            {evaluationModeHint}
          </p>
        </div>

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

        {/* max_hops — diversity/optimization: 1–2 hops; edge_candidate: 0–2 hops */}
        <div className="space-y-1">
          <Label>Max hops</Label>
          <ToggleGroup
            type="single"
            value={String(form.max_hops)}
            onValueChange={(next) => {
              if (!next) return;
              setForm((prev) => ({ ...prev, max_hops: Number(next) }));
            }}
            variant="outline"
            aria-label="Max hops"
            disabled={!isEligible || isPending}
          >
            {isEdgeCandidate && (
              <ToggleGroupItem value="0" aria-label="Direct only (0)">
                Direct only
              </ToggleGroupItem>
            )}
            <ToggleGroupItem value="1" aria-label="1 hop">
              1 hop
            </ToggleGroupItem>
            <ToggleGroupItem value="2" aria-label="2 hops">
              2 hops
            </ToggleGroupItem>
          </ToggleGroup>
        </div>

        {/* edge_candidate-only knobs */}
        {isEdgeCandidate && (
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="space-y-1">
              <Label htmlFor="settings-useful-latency-ms">
                Useful latency (ms){" "}
                <span className="text-destructive" aria-hidden="true">
                  *
                </span>
              </Label>
              <Input
                id="settings-useful-latency-ms"
                type="number"
                min={KNOB_BOUNDS.useful_latency_ms.min}
                max={KNOB_BOUNDS.useful_latency_ms.max}
                value={nullableKnobInputValue(form.useful_latency_ms)}
                placeholder="e.g. 80"
                aria-required="true"
                className={
                  form.useful_latency_ms === null
                    ? "border-destructive focus-visible:ring-destructive"
                    : undefined
                }
                onChange={handleNullable("useful_latency_ms")}
                disabled={!isEligible || isPending}
              />
              {form.useful_latency_ms === null && (
                <p className="text-xs text-destructive">Required for edge candidate mode.</p>
              )}
            </div>
            <div className="space-y-1">
              <Label htmlFor="settings-vm-lookback-minutes">Lookback window (min)</Label>
              <Input
                id="settings-vm-lookback-minutes"
                type="number"
                min={KNOB_BOUNDS.vm_lookback_minutes.min}
                max={KNOB_BOUNDS.vm_lookback_minutes.max}
                value={form.vm_lookback_minutes}
                onChange={handleNumber("vm_lookback_minutes")}
                disabled={!isEligible || isPending}
              />
            </div>
          </div>
        )}

        {/*
         * Guardrail knobs. Optional — empty input clears the local form
         * state. NOTE: PATCH semantics on the backend use `COALESCE($n,
         * col)` for these columns, mirroring `loss_threshold_ratio` and
         * `stddev_weight`; clearing an input here does NOT push the
         * column back to NULL on the server. To disable a guardrail
         * after it has been set, clone the campaign and start fresh.
         *
         * min_improvement_ms and min_improvement_ratio are hidden in
         * edge_candidate mode (they are diversity/optimization-only).
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
          {!isEdgeCandidate && (
            <>
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
            </>
          )}
        </div>

        <div className="flex items-center gap-3">
          <Button type="submit" disabled={isSubmitDisabled}>
            {isPending ? "Re-evaluating…" : "Re-evaluate"}
          </Button>
          {!isEligible ? (
            <p className="text-xs text-muted-foreground">
              Re-evaluate is available once the campaign is Completed or Evaluated.
            </p>
          ) : null}
        </div>
      </form>

      {snapshot ? (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span>
            Last evaluated {snapshot.evaluated_at} —{" "}
            {snapshot.candidates_good} of {snapshot.candidates_total} candidates qualified.
          </span>
          {/* Badge variant `secondary` is the closest to "muted" in the badge component */}
          {isLegacySnapshot && (
            <Badge
              variant="secondary"
              title="This evaluation pre-dates the max_hops/vm_lookback knobs."
            >
              legacy
            </Badge>
          )}
        </div>
      ) : null}

      {showSingleSourceBanner && (
        <div
          role="status"
          className="rounded-md border border-blue-200 bg-blue-50 px-3 py-2 text-sm dark:border-blue-800 dark:bg-blue-950"
        >
          This campaign has only one source agent — only direct routes from candidates to that agent
          are evaluated.
        </div>
      )}

      {/*
       * Operator-footgun warning. A tight guardrail set against a sparse
       * baseline can drop every candidate; the operator sees "0 of 0"
       * above and may assume the campaign produced no useful data. The
       * warning fires only when at least one guardrail is set on the
       * snapshot — an empty result set with no guardrails is a different
       * shape (likely no baseline pairs).
       */}
      {snapshot &&
      snapshot.candidates_total === 0 &&
      hasAnyGuardrailSet(snapshot) ? (
        <p className="text-xs text-amber-600 dark:text-amber-400" role="status">
          The active guardrails dropped every candidate. Loosen one or more knobs and re-evaluate to
          see results.
        </p>
      ) : null}
    </Card>
  );
}
