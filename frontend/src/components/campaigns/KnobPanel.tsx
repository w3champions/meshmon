import type { EvaluationMode } from "@/api/hooks/campaigns";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Toggle } from "@/components/ui/toggle";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import {
  type CampaignKnobs,
  clampKnob,
  KNOB_BOUNDS,
  type KnobProtocol,
  nullableKnobInputValue,
  parseNullableKnob,
  ratioToPercentInput,
} from "@/lib/campaign-config";

const MTR_HINT =
  "MTR is expensive — prefer ICMP/TCP/UDP here and use the per-pair Detail action in the results view.";
const FORCE_HELP =
  "When on, the 24 h reuse cache is ignored and the reusable count collapses to zero.";
const DIVERSITY_HINT =
  "Evaluator qualifies a transit agent X when A → X → B beats the direct A → B path. Broader result set; surfaces every viable alternative route.";
const OPTIMIZATION_HINT =
  "Evaluator qualifies X only when A → X → B beats direct AND every existing mesh transit. Tighter result set; surfaces the genuinely best candidates.";
const EDGE_CANDIDATE_HINT =
  "Evaluator scores X candidates by their direct + transitive (X → A → B) connectivity to a fixed set of mesh agents. Use to evaluate new edge-node locations.";
const MAX_HOPS_CAPTION =
  "2 hops considers an additional mesh agent in each route. Default for richer evaluation.";

export interface KnobPanelProps {
  value: CampaignKnobs;
  onChange(next: CampaignKnobs): void;
  /**
   * When true, every knob input is disabled. Set by the composer after the
   * draft campaign has been created and is waiting on the threshold-confirm
   * gate — further edits at that point would be discarded because the
   * draft's values are already server-side.
   */
  disabled?: boolean;
}

export function KnobPanel({ value, onChange, disabled = false }: KnobPanelProps) {
  const patch = (delta: Partial<CampaignKnobs>) => onChange({ ...value, ...delta });

  const isEdgeCandidate = value.evaluation_mode === "edge_candidate";

  type NumericKey =
    | "probe_count"
    | "probe_count_detail"
    | "timeout_ms"
    | "probe_stagger_ms"
    | "stddev_weight"
    | "vm_lookback_minutes";

  const handleNumber = (key: NumericKey) => (event: React.ChangeEvent<HTMLInputElement>) => {
    const raw = event.target.value;
    const parsed = raw === "" ? value[key] : Number(raw);
    patch({ [key]: clampKnob(key, parsed, value[key]) } as Partial<CampaignKnobs>);
  };

  type NullableKey =
    | "max_transit_rtt_ms"
    | "max_transit_stddev_ms"
    | "min_improvement_ms"
    | "min_improvement_ratio"
    | "useful_latency_ms";

  const handleNullable = (key: NullableKey) => (event: React.ChangeEvent<HTMLInputElement>) => {
    const next = parseNullableKnob(key, event.target.value, value[key] as number | null);
    patch({ [key]: next } as Partial<CampaignKnobs>);
  };

  const handleLossThresholdPct = (event: React.ChangeEvent<HTMLInputElement>) => {
    const raw = event.target.value;
    if (raw === "") return;
    const percent = Number(raw);
    if (!Number.isFinite(percent)) return;
    const ratio = percent / 100;
    patch({
      loss_threshold_ratio: clampKnob("loss_threshold_ratio", ratio, value.loss_threshold_ratio),
    });
  };

  const evaluationModeHint =
    value.evaluation_mode === "diversity"
      ? DIVERSITY_HINT
      : value.evaluation_mode === "edge_candidate"
        ? EDGE_CANDIDATE_HINT
        : OPTIMIZATION_HINT;

  return (
    <section
      aria-label="Campaign knobs"
      aria-disabled={disabled || undefined}
      className="flex flex-col gap-4"
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-1">
          <Label htmlFor="campaign-title">Title</Label>
          <Input
            id="campaign-title"
            value={value.title}
            placeholder="Campaign title"
            onChange={(e) => patch({ title: e.target.value })}
            disabled={disabled}
          />
        </div>
        <div className="space-y-1 sm:col-span-2">
          <Label htmlFor="campaign-notes">Notes</Label>
          <textarea
            id="campaign-notes"
            value={value.notes}
            placeholder="Operator notes…"
            onChange={(e) => patch({ notes: e.target.value })}
            rows={2}
            disabled={disabled}
            className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
          />
        </div>
      </div>

      <div className="space-y-1">
        <Label>Protocol</Label>
        <ToggleGroup
          type="single"
          value={value.protocol}
          onValueChange={(next) => {
            if (!next) return;
            patch({ protocol: next as KnobProtocol });
          }}
          variant="outline"
          aria-label="Probe protocol"
          disabled={disabled}
        >
          <ToggleGroupItem value="icmp" aria-label="ICMP">
            ICMP
          </ToggleGroupItem>
          <ToggleGroupItem value="tcp" aria-label="TCP">
            TCP
          </ToggleGroupItem>
          <ToggleGroupItem value="udp" aria-label="UDP">
            UDP
          </ToggleGroupItem>
          <ToggleGroupItem value="mtr" aria-label="MTR">
            MTR
          </ToggleGroupItem>
        </ToggleGroup>
        {value.protocol === "mtr" ? (
          <p role="status" className="text-xs text-amber-600 dark:text-amber-400">
            {MTR_HINT}
          </p>
        ) : null}
      </div>

      <div className="space-y-1">
        <Label>Evaluation mode</Label>
        <ToggleGroup
          type="single"
          value={value.evaluation_mode}
          onValueChange={(next) => {
            if (!next) return;
            const mode = next as EvaluationMode;
            const hopsFix =
              mode !== "edge_candidate" && value.max_hops === 0 ? { max_hops: 1 } : {};
            patch({ evaluation_mode: mode, ...hopsFix });
          }}
          variant="outline"
          aria-label="Evaluation mode"
          aria-describedby="knob-evaluation-mode-hint"
          disabled={disabled}
        >
          <ToggleGroupItem value="diversity" aria-label="diversity">
            Diversity
          </ToggleGroupItem>
          <ToggleGroupItem value="optimization" aria-label="optimization">
            Optimization
          </ToggleGroupItem>
          <ToggleGroupItem value="edge_candidate" aria-label="edge_candidate">
            Edge candidate
          </ToggleGroupItem>
        </ToggleGroup>
        <p id="knob-evaluation-mode-hint" className="text-xs text-muted-foreground">
          {evaluationModeHint}
        </p>
      </div>

      {/* max_hops — diversity/optimization: 1–2 hops; edge_candidate: 0–2 hops */}
      <div className="space-y-1">
        <Label>Max hops</Label>
        <ToggleGroup
          type="single"
          value={String(value.max_hops)}
          onValueChange={(next) => {
            if (!next) return;
            patch({ max_hops: Number(next) });
          }}
          variant="outline"
          aria-label="Max hops"
          disabled={disabled}
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
        {!isEdgeCandidate && <p className="text-xs text-muted-foreground">{MAX_HOPS_CAPTION}</p>}
      </div>

      {/* edge_candidate-only knobs */}
      {isEdgeCandidate && (
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="knob-useful-latency-ms">
              Useful latency (ms){" "}
              <span className="text-destructive" aria-hidden="true">
                *
              </span>
            </Label>
            <Input
              id="knob-useful-latency-ms"
              type="number"
              min={KNOB_BOUNDS.useful_latency_ms.min}
              max={KNOB_BOUNDS.useful_latency_ms.max}
              value={nullableKnobInputValue(value.useful_latency_ms)}
              placeholder="e.g. 80"
              aria-required="true"
              className={
                value.useful_latency_ms === null
                  ? "border-destructive focus-visible:ring-destructive"
                  : undefined
              }
              onChange={handleNullable("useful_latency_ms")}
              disabled={disabled}
            />
            {value.useful_latency_ms === null && (
              <p className="text-xs text-destructive">Required for edge candidate mode.</p>
            )}
          </div>
          <div className="space-y-1">
            <Label htmlFor="knob-vm-lookback-minutes">Lookback window (min)</Label>
            <Input
              id="knob-vm-lookback-minutes"
              type="number"
              min={KNOB_BOUNDS.vm_lookback_minutes.min}
              max={KNOB_BOUNDS.vm_lookback_minutes.max}
              value={value.vm_lookback_minutes}
              onChange={handleNumber("vm_lookback_minutes")}
              disabled={disabled}
            />
          </div>
        </div>
      )}

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-1">
          <Label htmlFor="knob-probe-count">Probe count</Label>
          <Input
            id="knob-probe-count"
            type="number"
            min={KNOB_BOUNDS.probe_count.min}
            max={KNOB_BOUNDS.probe_count.max}
            value={value.probe_count}
            onChange={handleNumber("probe_count")}
            disabled={disabled}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="knob-probe-count-detail">Probe count (detail)</Label>
          <Input
            id="knob-probe-count-detail"
            type="number"
            min={KNOB_BOUNDS.probe_count_detail.min}
            max={KNOB_BOUNDS.probe_count_detail.max}
            value={value.probe_count_detail}
            onChange={handleNumber("probe_count_detail")}
            disabled={disabled}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="knob-timeout-ms">Timeout (ms)</Label>
          <Input
            id="knob-timeout-ms"
            type="number"
            min={KNOB_BOUNDS.timeout_ms.min}
            max={KNOB_BOUNDS.timeout_ms.max}
            value={value.timeout_ms}
            onChange={handleNumber("timeout_ms")}
            disabled={disabled}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="knob-probe-stagger-ms">Probe stagger (ms)</Label>
          <Input
            id="knob-probe-stagger-ms"
            type="number"
            min={KNOB_BOUNDS.probe_stagger_ms.min}
            max={KNOB_BOUNDS.probe_stagger_ms.max}
            value={value.probe_stagger_ms}
            onChange={handleNumber("probe_stagger_ms")}
            disabled={disabled}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="knob-loss-threshold">Loss threshold (%)</Label>
          <Input
            id="knob-loss-threshold"
            type="number"
            step="0.1"
            min={KNOB_BOUNDS.loss_threshold_ratio.min * 100}
            max={KNOB_BOUNDS.loss_threshold_ratio.max * 100}
            value={ratioToPercentInput(value.loss_threshold_ratio)}
            onChange={handleLossThresholdPct}
            disabled={disabled}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="knob-stddev-weight">Stddev weight</Label>
          <Input
            id="knob-stddev-weight"
            type="number"
            step="0.1"
            min={KNOB_BOUNDS.stddev_weight.min}
            max={KNOB_BOUNDS.stddev_weight.max}
            value={value.stddev_weight}
            onChange={handleNumber("stddev_weight")}
            disabled={disabled}
          />
        </div>
      </div>

      {/*
       * Guardrail knobs (eligibility caps + storage floors). Optional —
       * leaving an input blank disables that gate. The two improvement
       * knobs accept negative values per spec. See `campaign-config.ts`
       * for bounds. Hidden in edge_candidate mode (those are diversity/optimization-only).
       */}
      <div className="space-y-1">
        <Label className="text-sm font-semibold">Evaluation guardrails (optional)</Label>
        <p id="knob-guardrails-hint" className="text-xs text-muted-foreground">
          Eligibility caps prune transit candidates before scoring; storage floors gate which
          per-pair rows are persisted (combined under OR semantics). Leave blank to disable.
        </p>
      </div>
      <div className="grid gap-3 sm:grid-cols-2" aria-describedby="knob-guardrails-hint">
        <div className="space-y-1">
          <Label htmlFor="knob-max-transit-rtt-ms">Max transit RTT (ms)</Label>
          <Input
            id="knob-max-transit-rtt-ms"
            type="number"
            min={KNOB_BOUNDS.max_transit_rtt_ms.min}
            max={KNOB_BOUNDS.max_transit_rtt_ms.max}
            value={nullableKnobInputValue(value.max_transit_rtt_ms)}
            placeholder="e.g. 200"
            onChange={handleNullable("max_transit_rtt_ms")}
            disabled={disabled}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="knob-max-transit-stddev-ms">Max transit RTT stddev (ms)</Label>
          <Input
            id="knob-max-transit-stddev-ms"
            type="number"
            min={KNOB_BOUNDS.max_transit_stddev_ms.min}
            max={KNOB_BOUNDS.max_transit_stddev_ms.max}
            value={nullableKnobInputValue(value.max_transit_stddev_ms)}
            placeholder="e.g. 50"
            onChange={handleNullable("max_transit_stddev_ms")}
            disabled={disabled}
          />
        </div>
        {!isEdgeCandidate && (
          <>
            <div className="space-y-1">
              <Label htmlFor="knob-min-improvement-ms">Min improvement (ms)</Label>
              <Input
                id="knob-min-improvement-ms"
                type="number"
                step="0.1"
                min={KNOB_BOUNDS.min_improvement_ms.min}
                max={KNOB_BOUNDS.min_improvement_ms.max}
                value={nullableKnobInputValue(value.min_improvement_ms)}
                placeholder="e.g. 5 (negative values allowed)"
                onChange={handleNullable("min_improvement_ms")}
                disabled={disabled}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="knob-min-improvement-ratio">Min improvement ratio</Label>
              <Input
                id="knob-min-improvement-ratio"
                type="number"
                step="0.01"
                min={KNOB_BOUNDS.min_improvement_ratio.min}
                max={KNOB_BOUNDS.min_improvement_ratio.max}
                value={nullableKnobInputValue(value.min_improvement_ratio)}
                placeholder="e.g. 0.1 (10%)"
                onChange={handleNullable("min_improvement_ratio")}
                disabled={disabled}
              />
            </div>
          </>
        )}
      </div>

      <div className="space-y-1">
        <Toggle
          pressed={value.force_measurement}
          onPressedChange={(pressed) => patch({ force_measurement: pressed })}
          variant="outline"
          aria-label="Force measurement"
          aria-describedby="knob-force-hint"
          disabled={disabled}
        >
          Force measurement
        </Toggle>
        <p id="knob-force-hint" className="text-xs text-muted-foreground">
          {FORCE_HELP}
        </p>
      </div>
    </section>
  );
}
