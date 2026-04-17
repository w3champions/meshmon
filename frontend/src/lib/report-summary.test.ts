import { describe, expect, it } from "vitest";
import type { RouteSnapshotDetail } from "@/api/hooks/route-snapshot";
import { buildReportSummary } from "./report-summary";

function snap(
  id: number,
  hops: Array<{ position: number; ip: string; rtt_us: number; loss: number }>,
): RouteSnapshotDetail {
  return {
    id,
    source_id: "br-a",
    target_id: "fr-a",
    protocol: "icmp",
    observed_at: "2026-04-13T10:00:00Z",
    path_summary: null,
    hops: hops.map((h) => ({
      position: h.position,
      avg_rtt_micros: h.rtt_us,
      loss_pct: h.loss,
      observed_ips: [{ ip: h.ip, freq: 1 }],
      stddev_rtt_micros: 0,
    })),
  };
}

describe("buildReportSummary", () => {
  it("reports route-changed when hop IPs differ", () => {
    const before = snap(1, [
      { position: 1, ip: "10.0.0.1", rtt_us: 1000, loss: 0 },
      { position: 2, ip: "10.0.0.2", rtt_us: 2000, loss: 0 },
    ]);
    const after = snap(2, [
      { position: 1, ip: "10.0.0.1", rtt_us: 1000, loss: 0 },
      { position: 2, ip: "10.0.9.9", rtt_us: 2500, loss: 0 },
    ]);
    const s = buildReportSummary({
      before,
      after,
      metricsFirst: { rtt_ms: 50, loss: 0.001 },
      metricsLast: { rtt_ms: 120, loss: 0.05 },
    });
    expect(s.routeChanged).toBe(true);
    expect(s.rttBeforeMs).toBeCloseTo(50);
    expect(s.rttAfterMs).toBeCloseTo(120);
    // (120-50)/50 * 100 = 140
    expect(s.rttDeltaPct).toBeCloseTo(140);
    expect(s.lossBeforePct).toBeCloseTo(0.1);
    expect(s.lossAfterPct).toBeCloseTo(5);
    expect(s.lossDeltaPct).toBeCloseTo(4.9);
    expect(s.singleSnapshot).toBe(false);
  });

  it("reports no route change when hops match", () => {
    const hops = [{ position: 1, ip: "10.0.0.1", rtt_us: 1000, loss: 0 }];
    const before = snap(1, hops);
    const after = snap(2, hops);
    expect(
      buildReportSummary({
        before,
        after,
        metricsFirst: null,
        metricsLast: null,
      }).routeChanged,
    ).toBe(false);
  });

  it("returns nulls when metrics are unavailable", () => {
    const hops = [{ position: 1, ip: "10.0.0.1", rtt_us: 1000, loss: 0 }];
    const s = buildReportSummary({
      before: snap(1, hops),
      after: snap(1, hops),
      metricsFirst: null,
      metricsLast: null,
    });
    expect(s.rttBeforeMs).toBeNull();
    expect(s.rttAfterMs).toBeNull();
    expect(s.rttDeltaPct).toBeNull();
    expect(s.lossBeforePct).toBeNull();
    expect(s.lossAfterPct).toBeNull();
    expect(s.lossDeltaPct).toBeNull();
  });

  it("single-snapshot windows return singleSnapshot=true and routeChanged=false", () => {
    const hops = [{ position: 1, ip: "10.0.0.1", rtt_us: 1000, loss: 0 }];
    const only = snap(1, hops);
    const s = buildReportSummary({
      before: only,
      after: only,
      metricsFirst: null,
      metricsLast: null,
    });
    expect(s.routeChanged).toBe(false);
    expect(s.singleSnapshot).toBe(true);
  });
});
