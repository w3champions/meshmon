import type { EvaluationMode, ProbeProtocol } from "@/api/hooks/campaigns";

/**
 * Keep in sync with backend `[campaigns] size_warning_threshold`
 * (crates/service/src/config.rs:92). When this threshold is exceeded,
 * the composer surfaces a confirmation dialog before starting.
 *
 * The backend does not expose this value over HTTP today; a follow-up
 * task could add `/api/config/public` if runtime-driven overrides are
 * ever needed.
 */
export const SIZE_WARNING_THRESHOLD = 1000;

/**
 * Protocol values the operator can pick in the composer. `mtr` is a
 * UI-only sentinel — selecting it disables the Start button and surfaces
 * an inline hint. The backend `ProbeProtocol` enum is `icmp | tcp | udp`;
 * operators run MTR via the per-pair Detail action in the results view.
 */
export type KnobProtocol = ProbeProtocol | "mtr";

/**
 * Fully-specified knob draft the composer edits as a controlled value.
 * Defaults match the backend INSERT (`crates/service/src/campaign/repo.rs`
 * lines 112-115): `probe_count=10`, `probe_count_detail=250`,
 * `timeout_ms=2000`, `probe_stagger_ms=100`, `loss_threshold_ratio=0.02`,
 * `stddev_weight=1.0`, `evaluation_mode='optimization'`.
 *
 * `loss_threshold_ratio` is a fraction in `[0, 1]` on the wire; the knob
 * form presents the same value as percent in the UX layer.
 */
export interface CampaignKnobs {
  title: string;
  notes: string;
  protocol: KnobProtocol;
  /** Probes per dispatched measurement (campaign rounds). */
  probe_count: number;
  /** Probes per detail measurement (UI re-runs). */
  probe_count_detail: number;
  /** Per-probe timeout in milliseconds. */
  timeout_ms: number;
  /** Inter-probe stagger in milliseconds. */
  probe_stagger_ms: number;
  /** Loss-rate threshold (fraction 0.0–1.0) used by the evaluator. */
  loss_threshold_ratio: number;
  /** Weight applied to RTT stddev by the evaluator. */
  stddev_weight: number;
  /** Evaluation strategy. */
  evaluation_mode: EvaluationMode;
  /**
   * Optional eligibility cap on composed transit RTT (ms). `null` disables
   * the gate. Pruning happens before counter accumulation.
   */
  max_transit_rtt_ms: number | null;
  /**
   * Optional eligibility cap on composed transit RTT stddev (ms). `null`
   * disables the gate.
   */
  max_transit_stddev_ms: number | null;
  /**
   * Optional storage floor on absolute improvement (ms). `null` disables
   * the gate. Combined with `min_improvement_ratio` under OR semantics.
   * Negative values are accepted to allow "near-baseline" rows through.
   */
  min_improvement_ms: number | null;
  /**
   * Optional storage floor on relative improvement (fraction). `null`
   * disables the gate. Negative values are accepted by parity with the
   * absolute knob.
   */
  min_improvement_ratio: number | null;
  /** When true, the scheduler ignores the 24 h reuse cache. */
  force_measurement: boolean;
  /**
   * RTT threshold (ms) below which a route qualifies as "useful" in
   * edge_candidate mode. Required for edge_candidate; ignored otherwise.
   * `null` means not set (invalid for edge_candidate, omitted on the wire
   * for diversity/optimization).
   */
  useful_latency_ms: number | null;
  /**
   * Maximum transit hops for edge_candidate route enumeration.
   * Range [0, 2]. Default 2.
   */
  max_hops: number;
  /**
   * Look-back window (minutes) for VictoriaMetrics data in edge_candidate
   * mode. Range [1, 1440]. Default 15.
   */
  vm_lookback_minutes: number;
}

/**
 * Min/max clamps for numeric knobs. The lower bound is 1 for everything
 * but `probe_stagger_ms` (0 is legitimate — dispatch with no stagger)
 * and `loss_threshold_ratio` / `stddev_weight` (both accept 0).
 *
 * `loss_threshold_ratio` is clamped in ratio units (0.0–1.0); the composer
 * input is percent-facing and converts at the form boundary.
 *
 * The four guardrail knobs are nullable on the wire (`null` means "gate
 * disabled"). The bounds below clamp non-null user input; the form layer
 * preserves `null` as an out-of-band "off" value via `parseNullableKnob`.
 * `min_improvement_ms` / `min_improvement_ratio` accept negative input —
 * the spec deliberately allows operators to keep "near-baseline" rows
 * (e.g. transit X is 5 ms slower but more stable) by setting a negative
 * floor.
 */
export const KNOB_BOUNDS: Record<
  | "probe_count"
  | "probe_count_detail"
  | "timeout_ms"
  | "probe_stagger_ms"
  | "loss_threshold_ratio"
  | "stddev_weight"
  | "max_transit_rtt_ms"
  | "max_transit_stddev_ms"
  | "min_improvement_ms"
  | "min_improvement_ratio"
  | "useful_latency_ms"
  | "max_hops"
  | "vm_lookback_minutes",
  { min: number; max: number }
> = {
  probe_count: { min: 1, max: 1000 },
  probe_count_detail: { min: 1, max: 10000 },
  timeout_ms: { min: 100, max: 60000 },
  probe_stagger_ms: { min: 0, max: 60000 },
  loss_threshold_ratio: { min: 0, max: 1 },
  stddev_weight: { min: 0, max: 10 },
  max_transit_rtt_ms: { min: 1, max: 10000 },
  max_transit_stddev_ms: { min: 0, max: 5000 },
  min_improvement_ms: { min: -10000, max: 10000 },
  min_improvement_ratio: { min: -1, max: 1 },
  useful_latency_ms: { min: 1, max: 10000 },
  max_hops: { min: 0, max: 2 },
  vm_lookback_minutes: { min: 1, max: 1440 },
};

/**
 * Fresh knob draft used when the composer mounts. Never mutated — callers
 * that need to edit a field produce a shallow copy via spread.
 *
 * The four guardrail knobs default to `null` (gate disabled) on a fresh
 * draft. Operators opt in per-knob; an unset guardrail mirrors the
 * backend default of "no eligibility cap, no storage floor".
 */
export const DEFAULT_KNOBS: CampaignKnobs = {
  title: "",
  notes: "",
  protocol: "icmp",
  probe_count: 10,
  probe_count_detail: 250,
  timeout_ms: 2000,
  probe_stagger_ms: 100,
  loss_threshold_ratio: 0.02,
  stddev_weight: 1.0,
  evaluation_mode: "optimization",
  max_transit_rtt_ms: null,
  max_transit_stddev_ms: null,
  min_improvement_ms: null,
  min_improvement_ratio: null,
  force_measurement: false,
  useful_latency_ms: null,
  max_hops: 2,
  vm_lookback_minutes: 15,
};

/**
 * Clamp a numeric knob to its configured min/max. Returns the fallback
 * when the input is not finite (e.g. the operator cleared the field),
 * which keeps downstream callers free of NaN propagation.
 */
export function clampKnob(key: keyof typeof KNOB_BOUNDS, value: number, fallback: number): number {
  if (!Number.isFinite(value)) return fallback;
  const { min, max } = KNOB_BOUNDS[key];
  if (value < min) return min;
  if (value > max) return max;
  return value;
}

/**
 * Render a ratio knob as a percent number suitable for an `<input type="number">`
 * value. Rounds to four decimals so float-multiplication artefacts
 * (`0.075 * 100 → 7.500000000000001`) never leak into the rendered form.
 */
export function ratioToPercentInput(ratio: number): number {
  return Math.round(ratio * 1_000_000) / 10_000;
}

/**
 * Parse a guardrail-style knob whose "off" value is `null` rather than a
 * numeric default. Empty input collapses to `null`, non-finite input
 * holds the previous value, and finite input is clamped to the knob's
 * configured bounds. The `prev ?? KNOB_BOUNDS[key].min` fallback gives
 * `clampKnob` something sensible when transitioning from `null` →
 * a typed value.
 */
export function parseNullableKnob(
  key: keyof typeof KNOB_BOUNDS,
  raw: string,
  prev: number | null,
): number | null {
  if (raw === "") return null;
  const n = Number(raw);
  if (!Number.isFinite(n)) return prev;
  return clampKnob(key, n, prev ?? KNOB_BOUNDS[key].min);
}

/**
 * Render a nullable guardrail knob as a controlled `<input type="number">`
 * value: `null` → `""` so the field renders empty (matching the "off"
 * sentinel), any number passes through unchanged.
 */
export function nullableKnobInputValue(n: number | null): number | string {
  return n === null ? "" : n;
}
