/**
 * RouteLegRow — renders a single leg within a composed route with provenance chips.
 *
 * Chip variants (spec §7.3):
 * - `was_substituted = true` → yellow chip "← reverse-substituted (ingress block detected)"
 *   with tooltip explaining ingress filtering. If the leg's loss also exceeds the campaign's
 *   loss_threshold_ratio, an additional "exceeds loss threshold" chip renders alongside.
 * - `source = symmetric_reuse` AND `was_substituted = false` → gray chip "← symmetric reuse".
 * - All other cases → no chip.
 */

import type { components } from "@/api/schema.gen";

// Re-export so test files can import the type from a single location.
export type LegDto = components["schemas"]["LegDto"];
export type LegSource = components["schemas"]["LegSource"];

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface RouteLegRowProps {
  leg: LegDto;
  /**
   * Campaign's `loss_threshold_ratio` (fraction 0.0–1.0). Used to determine
   * whether a substituted leg's loss also exceeds the campaign threshold, in
   * which case a second chip is rendered alongside the substitution chip.
   * Per spec §6.6, substitution does NOT bypass the loss threshold gate.
   */
  lossThresholdRatio: number;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function RouteLegRow({ leg, lossThresholdRatio }: RouteLegRowProps) {
  const showSubstituted = leg.was_substituted;
  const showSymmetricReuse = !leg.was_substituted && leg.source === "symmetric_reuse";
  const showExceedsLoss = leg.was_substituted && leg.loss_ratio > lossThresholdRatio;

  return (
    <div className="flex flex-wrap items-center gap-2 py-1 text-sm">
      {/* Endpoint pair */}
      <span className="font-mono text-xs text-muted-foreground">
        {leg.from_id}
        <span className="mx-1 text-muted-foreground/60">→</span>
        {leg.to_id}
      </span>

      {/* Metrics */}
      <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
        <span>{leg.rtt_ms.toFixed(1)} ms</span>
        <span className="text-muted-foreground/50">±</span>
        <span>{leg.stddev_ms.toFixed(1)}</span>
        {leg.loss_ratio > 0 && (
          <>
            <span className="text-muted-foreground/50">·</span>
            <span className="text-destructive">{(leg.loss_ratio * 100).toFixed(1)}% loss</span>
          </>
        )}
      </span>

      {/* Provenance chips */}
      {showSubstituted && (
        <span
          className="inline-flex items-center rounded-full bg-yellow-500/20 text-yellow-700 dark:text-yellow-400 px-2 py-0.5 text-xs font-medium"
          title="This leg's data comes from the reverse measurement direction because the forward direction showed 100% packet loss (ingress block). The substituted measurement is used in its place."
        >
          ← reverse-substituted (ingress block detected)
        </span>
      )}
      {showExceedsLoss && (
        <span className="inline-flex items-center rounded-full bg-destructive/20 text-destructive px-2 py-0.5 text-xs font-medium">
          exceeds loss threshold
        </span>
      )}
      {showSymmetricReuse && (
        <span className="inline-flex items-center rounded-full bg-muted text-muted-foreground px-2 py-0.5 text-xs font-medium">
          ← symmetric reuse
        </span>
      )}
    </div>
  );
}
