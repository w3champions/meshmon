/**
 * Shared helpers for the `MeasurementSource` wire enum.
 *
 * Rows on the Raw / Pairs tabs carry `source`: either `active_probe` (the
 * agent actually measured this pair during the campaign run) or
 * `archived_vm_continuous` (the evaluator archived a VictoriaMetrics
 * continuous-mesh sample so the agent mesh doesn't have to re-probe itself).
 *
 * The helpers normalise null / undefined to `active_probe` — the backend
 * default — so render sites can feed raw DTO fields in without defensive
 * null-coalescing at every call site.
 */

import type { components } from "@/api/schema.gen";

export type MeasurementSource = components["schemas"]["MeasurementSource"];

/**
 * Human-facing label. Kept deliberately short so it renders inside a badge;
 * the longer operator-facing description lives in tooltips at each call
 * site when needed.
 */
export function measurementSourceLabel(source: MeasurementSource | null | undefined): string {
  if (source === "archived_vm_continuous") return "baseline (VM)";
  return "active probe";
}

/**
 * Tailwind class bundle for the source badge. VM-archived rows render in a
 * sky/blue tone so operators can visually separate baseline-from-mesh rows
 * from active-probe rows at a glance; active-probe rows stay muted so the
 * default case doesn't add visual noise.
 */
export function measurementSourceBadgeClass(source: MeasurementSource | null | undefined): string {
  if (source === "archived_vm_continuous") {
    return "bg-sky-500/15 text-sky-700 dark:text-sky-300";
  }
  return "bg-muted text-muted-foreground";
}

/**
 * Normalise a possibly-absent `source` field to the default `active_probe`.
 * Pending / dispatched rows that have no joined `measurements` row yet can
 * surface here without the caller branching.
 */
export function normaliseSource(source: MeasurementSource | null | undefined): MeasurementSource {
  return source ?? "active_probe";
}
