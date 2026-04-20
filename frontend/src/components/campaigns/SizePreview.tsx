import { useEffect, useRef } from "react";
import { usePreviewDispatchCount } from "@/api/hooks/campaigns";
import { Skeleton } from "@/components/ui/skeleton";

export interface SizePreviewProps {
  /** Source agents currently in the composer's `selected` set. */
  sourcesSelected: number;
  /**
   * Pre-submit destination count (first-page total or the size of the
   * `selected` IP set) used for the naïve product. Ignored in phase 2.
   */
  approxTotal: number;
  /**
   * True when the operator's filter includes at least one map shape. The
   * pre-submit product is then approximate rather than exact, so we
   * surface a leading `~`.
   */
  shapesActive: boolean;
  /**
   * When present, the component switches to phase 2 and polls the
   * backend preview endpoint for an authoritative total / reusable /
   * fresh breakdown.
   */
  campaignId?: string;
  /**
   * Forwarded from the composer's knob panel. Mirrors the backend's
   * `force_measurement` flag; when true the reusable count is surfaced
   * as zero in the UI (defense-in-depth; the backend preview already
   * respects the flag).
   */
  forceMeasurement: boolean;
  /**
   * Strict cutoff above which the composer prompts for confirmation
   * before dispatch. Sourced from `SIZE_WARNING_THRESHOLD`.
   */
  sizeWarningThreshold: number;
  /**
   * Fires once per `fresh > threshold` crossing. Guarded by a ref so
   * repeated renders of the same crossed state don't re-trigger the
   * composer's confirm dialog.
   */
  onThresholdExceeded?(): void;
}

export function SizePreview({
  sourcesSelected,
  approxTotal,
  shapesActive,
  campaignId,
  forceMeasurement,
  sizeWarningThreshold,
  onThresholdExceeded,
}: SizePreviewProps) {
  // Phase 2 calls the preview hook; when `campaignId` is undefined the
  // hook stays disabled (see `usePreviewDispatchCount`), so this call
  // is safe in phase 1 too.
  const preview = usePreviewDispatchCount(campaignId);
  const data = preview.data;

  // Guard the threshold callback so a stable "over threshold" render
  // doesn't re-fire every refetch. Reset the latch when the condition
  // drops below threshold so a later re-crossing triggers again.
  const exceededRef = useRef(false);
  useEffect(() => {
    if (!data || !onThresholdExceeded) return;
    const crossed = data.fresh > sizeWarningThreshold;
    if (crossed && !exceededRef.current) {
      exceededRef.current = true;
      onThresholdExceeded();
    } else if (!crossed && exceededRef.current) {
      exceededRef.current = false;
    }
  }, [data, sizeWarningThreshold, onThresholdExceeded]);

  // Phase 1 — pre-submit: naïve product with optional `~` prefix.
  if (!campaignId) {
    const product = sourcesSelected * approxTotal;
    const prefix = shapesActive ? "~" : "";
    return (
      <div aria-live="polite" className="rounded-md border bg-muted/20 p-3 text-sm">
        <p>
          Expected: {prefix}
          {product} measurements
        </p>
      </div>
    );
  }

  // Phase 2 — post-submit: authoritative preview.
  if (preview.isLoading || !data) {
    return (
      <div className="rounded-md border bg-muted/20 p-3 text-sm" aria-live="polite">
        <Skeleton className="h-4 w-56" />
      </div>
    );
  }

  const forceOverride = forceMeasurement && data.reusable > 0;
  return (
    <div aria-live="polite" className="rounded-md border bg-muted/20 p-3 text-sm">
      {forceOverride ? (
        <>
          <p>
            Expected: {data.fresh} = {data.total} measurements
          </p>
          <p className="text-xs text-muted-foreground">
            Force measurement is on — reusable count shown as zero.
          </p>
        </>
      ) : (
        <p>
          Expected: {data.total} measurements ({data.reusable} reusable from last 24 h, {data.fresh}{" "}
          new).
        </p>
      )}
    </div>
  );
}
