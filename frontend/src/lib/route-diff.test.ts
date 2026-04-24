import { describe, expect, test } from "vitest";
import type { components } from "@/api/schema.gen";
import { computeRouteDiff, type HopChangeKind } from "@/lib/route-diff";

type HopJson = components["schemas"]["HopJson"];

function hop(
  position: number,
  ip: string,
  avgRttMicros: number,
  lossPct = 0,
  stddevRttMicros = 100,
): HopJson {
  return {
    position,
    observed_ips: [{ ip, freq: 1 }],
    avg_rtt_micros: avgRttMicros,
    stddev_rtt_micros: stddevRttMicros,
    loss_ratio: lossPct,
  };
}

describe("computeRouteDiff", () => {
  test("identical routes produce all unchanged hops", () => {
    const a = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.2", 2_000)];
    const diff = computeRouteDiff(a, a);
    expect(diff.summary).toEqual({
      totalHops: 2,
      changedHops: 0,
      addedHops: 0,
      removedHops: 0,
      firstChangedPosition: null,
    });
    expect(diff.perHop.get(1)?.kind).toBe("unchanged" satisfies HopChangeKind);
    expect(diff.perHop.get(2)?.kind).toBe("unchanged" satisfies HopChangeKind);
  });

  test("ip change at a single hop", () => {
    const a = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.2", 2_000)];
    const b = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.99", 2_000)];
    const diff = computeRouteDiff(a, b);
    expect(diff.perHop.get(2)?.kind).toBe("ip_changed");
    expect(diff.summary.changedHops).toBe(1);
    expect(diff.summary.firstChangedPosition).toBe(2);
  });

  test("latency change >= 50%", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    const b = [hop(1, "10.0.0.1", 1_600)];
    expect(computeRouteDiff(a, b).perHop.get(1)?.kind).toBe("latency_changed");
  });

  test("latency change below 50% is unchanged", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    const b = [hop(1, "10.0.0.1", 1_400)];
    expect(computeRouteDiff(a, b).perHop.get(1)?.kind).toBe("unchanged");
  });

  test("ip + latency both changed", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    const b = [hop(1, "10.0.0.9", 1_800)];
    expect(computeRouteDiff(a, b).perHop.get(1)?.kind).toBe("both_changed");
  });

  test("added hop", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    const b = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.2", 2_000)];
    const diff = computeRouteDiff(a, b);
    expect(diff.perHop.get(2)?.kind).toBe("added");
    expect(diff.summary.addedHops).toBe(1);
    expect(diff.summary.firstChangedPosition).toBe(2);
  });

  test("removed hop", () => {
    const a = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.2", 2_000)];
    const b = [hop(1, "10.0.0.1", 1_000)];
    const diff = computeRouteDiff(a, b);
    expect(diff.perHop.get(2)?.kind).toBe("removed");
    expect(diff.summary.removedHops).toBe(1);
  });

  test("dominant ip follows highest frequency", () => {
    const twoIps: HopJson = {
      position: 1,
      observed_ips: [
        { ip: "10.0.0.1", freq: 3 },
        { ip: "10.0.0.2", freq: 7 },
      ],
      avg_rtt_micros: 1_000,
      stddev_rtt_micros: 100,
      loss_ratio: 0,
    };
    const a = [twoIps];
    const b = [{ ...twoIps, observed_ips: [{ ip: "10.0.0.1", freq: 10 }] }];
    const diff = computeRouteDiff(a, b);
    expect(diff.perHop.get(1)?.aDominantIp).toBe("10.0.0.2");
    expect(diff.perHop.get(1)?.bDominantIp).toBe("10.0.0.1");
    expect(diff.perHop.get(1)?.kind).toBe("ip_changed");
  });

  test("firstChangedPosition is null for identical routes", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    expect(computeRouteDiff(a, a).summary.firstChangedPosition).toBeNull();
  });
});
