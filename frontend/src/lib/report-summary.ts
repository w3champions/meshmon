import type { RouteSnapshotDetail } from "@/api/hooks/route-snapshot";
import { computeRouteDiff } from "./route-diff";

export interface MetricsPoint {
  /** Milliseconds — already converted from micros by the caller. */
  rtt_ms: number;
  /** 0..1 loss fraction. */
  loss: number;
}

export interface ReportSummary {
  rttBeforeMs: number | null;
  rttAfterMs: number | null;
  /** (after - before) / before * 100, or null if either endpoint is missing. */
  rttDeltaPct: number | null;
  /** Percentage (0..100). */
  lossBeforePct: number | null;
  /** Percentage (0..100). */
  lossAfterPct: number | null;
  /** Percentage delta (after - before). */
  lossDeltaPct: number | null;
  routeChanged: boolean;
  singleSnapshot: boolean;
}

export interface BuildReportSummaryInput {
  before: RouteSnapshotDetail;
  after: RouteSnapshotDetail;
  metricsFirst: MetricsPoint | null;
  metricsLast: MetricsPoint | null;
}

export function buildReportSummary(
  input: BuildReportSummaryInput,
): ReportSummary {
  const { before, after, metricsFirst, metricsLast } = input;

  const singleSnapshot = before.id === after.id;
  const routeChanged = (() => {
    if (singleSnapshot) return false;
    const { changedHops, addedHops, removedHops } = computeRouteDiff(
      before.hops,
      after.hops,
    ).summary;
    return changedHops + addedHops + removedHops > 0;
  })();

  const rttBeforeMs = metricsFirst ? metricsFirst.rtt_ms : null;
  const rttAfterMs = metricsLast ? metricsLast.rtt_ms : null;
  const rttDeltaPct =
    rttBeforeMs !== null && rttAfterMs !== null && rttBeforeMs > 0
      ? ((rttAfterMs - rttBeforeMs) / rttBeforeMs) * 100
      : null;

  const lossBeforePct = metricsFirst ? metricsFirst.loss * 100 : null;
  const lossAfterPct = metricsLast ? metricsLast.loss * 100 : null;
  const lossDeltaPct =
    lossBeforePct !== null && lossAfterPct !== null
      ? lossAfterPct - lossBeforePct
      : null;

  return {
    rttBeforeMs,
    rttAfterMs,
    rttDeltaPct,
    lossBeforePct,
    lossAfterPct,
    lossDeltaPct,
    routeChanged,
    singleSnapshot,
  };
}
