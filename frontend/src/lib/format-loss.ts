/**
 * Formatting helpers for loss values.
 *
 * Backend stores loss as a ratio in `[0.0, 1.0]` (e.g. `0.0857` for ~8.57%).
 * The UI always presents percent, so every display site multiplies by 100
 * at render time. Thresholds are expressed in the same ratio units as the
 * wire values; the evaluator's `loss_threshold_ratio` is directly comparable
 * to `avg_loss_ratio` without rescaling.
 */

/**
 * Format a loss ratio (0.0–1.0) as a percent string with two decimals,
 * e.g. `0.0857 → "8.57%"`. Nullish values render as an em dash placeholder.
 */
export function formatLossRatio(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  return `${(value * 100).toFixed(2)}%`;
}

/**
 * Well-known loss-ratio thresholds used by the campaign results UI.
 *
 * - `healthy` → green below 0.5%
 * - `degraded` → amber below 2% (also the default `loss_threshold_ratio` on
 *   `CampaignDto` / `EvaluationDto`; see backend `crates/service/src/campaign/repo.rs`)
 *
 * Per-campaign evaluator thresholds override these when present — the
 * `CandidateTable.lossClass` check reads `evaluation.loss_threshold_ratio`
 * and compares directly against `avg_loss_ratio` in the same units.
 */
export const LOSS_RATIO_THRESHOLDS = {
  healthy: 0.005,
  degraded: 0.02,
} as const;
