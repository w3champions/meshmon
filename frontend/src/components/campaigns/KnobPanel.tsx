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
  ratioToPercentInput,
} from "@/lib/campaign-config";

const MTR_HINT =
  "MTR is expensive — prefer ICMP/TCP/UDP here and use the per-pair Detail action in the results view.";
const FORCE_HELP =
  "When on, the 24 h reuse cache is ignored and the reusable count collapses to zero.";
// Diversity and Optimization describe what the evaluator does with the
// measurements this campaign collects. Operators pick the mode up front so
// the evaluator can score candidates without a re-dispatch. Hints below
// summarise the predicate each mode applies — full semantics in spec 04 §2.
const DIVERSITY_HINT =
  "Evaluator qualifies a transit agent X when A → X → B beats the direct A → B path. Broader result set; surfaces every viable alternative route.";
const OPTIMIZATION_HINT =
  "Evaluator qualifies X only when A → X → B beats direct AND every existing mesh transit. Tighter result set; surfaces the genuinely best candidates.";

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

  const handleNumber =
    (key: keyof typeof KNOB_BOUNDS) => (event: React.ChangeEvent<HTMLInputElement>) => {
      const raw = event.target.value;
      // When the operator clears the field, preserve the fallback so the
      // knob stays at the current value rather than becoming NaN.
      const parsed = raw === "" ? value[key] : Number(raw);
      patch({ [key]: clampKnob(key, parsed, value[key]) } as Partial<CampaignKnobs>);
    };

  // `loss_threshold_ratio` is wire-format ratio (0.0–1.0), but the form UX
  // presents percent — convert at the form boundary so the DTO stays in
  // ratio units while the operator still types "2" for 2 %.
  const handleLossThresholdPct = (event: React.ChangeEvent<HTMLInputElement>) => {
    const raw = event.target.value;
    if (raw === "") {
      // Operator cleared the field — hold the current knob value.
      return;
    }
    const percent = Number(raw);
    if (!Number.isFinite(percent)) return;
    const ratio = percent / 100;
    patch({
      loss_threshold_ratio: clampKnob("loss_threshold_ratio", ratio, value.loss_threshold_ratio),
    });
  };

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
            // Radix emits an empty string when the active item is clicked
            // again; treat that as "keep current" so the knob never goes
            // blank under us.
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
            patch({ evaluation_mode: next as EvaluationMode });
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
        </ToggleGroup>
        <p id="knob-evaluation-mode-hint" className="text-xs text-muted-foreground">
          {value.evaluation_mode === "diversity" ? DIVERSITY_HINT : OPTIMIZATION_HINT}
        </p>
      </div>

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
