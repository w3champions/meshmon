import { describe, expect, test } from "vitest";
import type { components } from "@/api/schema.gen";
import { diffRouteSnapshots } from "@/lib/route-diff";

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
    loss_pct: lossPct,
  };
}

describe("diffRouteSnapshots", () => {
  test("unchanged hops yield no changes", () => {
    const a = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.2", 2_000)];
    const b = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.2", 2_000)];
    const diff = diffRouteSnapshots(a, b);
    expect(diff.added).toEqual([]);
    expect(diff.removed).toEqual([]);
    expect(diff.changed).toEqual([]);
    expect(diff.unchanged.map((h) => h.position)).toEqual([1, 2]);
  });

  test("detects removed hops (present in A, missing in B)", () => {
    const a = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.2", 2_000)];
    const b = [hop(1, "10.0.0.1", 1_000)];
    const diff = diffRouteSnapshots(a, b);
    expect(diff.removed.map((h) => h.position)).toEqual([2]);
    expect(diff.added).toEqual([]);
    expect(diff.changed).toEqual([]);
  });

  test("detects added hops (present in B, missing in A)", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    const b = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.2", 2_000)];
    const diff = diffRouteSnapshots(a, b);
    expect(diff.added.map((h) => h.position)).toEqual([2]);
    expect(diff.removed).toEqual([]);
    expect(diff.changed).toEqual([]);
  });

  test("detects changed hops when dominant IP changes", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    const b = [hop(1, "10.0.0.99", 1_000)];
    const diff = diffRouteSnapshots(a, b);
    expect(diff.changed).toHaveLength(1);
    expect(diff.changed[0].position).toBe(1);
    expect(diff.changed[0].from.observed_ips[0].ip).toBe("10.0.0.1");
    expect(diff.changed[0].to.observed_ips[0].ip).toBe("10.0.0.99");
    expect(diff.added).toEqual([]);
    expect(diff.removed).toEqual([]);
  });

  test("dominant IP picks the highest frequency entry, not just the first", () => {
    const twoIps: HopJson = {
      position: 1,
      observed_ips: [
        { ip: "10.0.0.1", freq: 3 },
        { ip: "10.0.0.2", freq: 7 },
      ],
      avg_rtt_micros: 1_000,
      stddev_rtt_micros: 100,
      loss_pct: 0,
    };
    const sameDominant: HopJson = {
      position: 1,
      observed_ips: [{ ip: "10.0.0.2", freq: 1 }],
      avg_rtt_micros: 1_000,
      stddev_rtt_micros: 100,
      loss_pct: 0,
    };
    // Dominant IP in A is 10.0.0.2 (freq 7); B's dominant is also 10.0.0.2,
    // so no change.
    const diff = diffRouteSnapshots([twoIps], [sameDominant]);
    expect(diff.changed).toEqual([]);
    expect(diff.unchanged).toHaveLength(1);
  });

  test("RTT delta > 20% is treated as changed", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    const b = [hop(1, "10.0.0.1", 1_300)];
    const diff = diffRouteSnapshots(a, b);
    expect(diff.changed).toHaveLength(1);
    expect(diff.changed[0].position).toBe(1);
  });

  test("RTT delta <= 20% is unchanged", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    const b = [hop(1, "10.0.0.1", 1_150)];
    const diff = diffRouteSnapshots(a, b);
    expect(diff.changed).toEqual([]);
    expect(diff.unchanged).toHaveLength(1);
  });

  test("loss_pct change > 0.05 (absolute) is changed", () => {
    const a = [hop(1, "10.0.0.1", 1_000, 0.0)];
    const b = [hop(1, "10.0.0.1", 1_000, 0.1)];
    const diff = diffRouteSnapshots(a, b);
    expect(diff.changed).toHaveLength(1);
  });

  test("loss_pct change <= 0.05 is unchanged", () => {
    const a = [hop(1, "10.0.0.1", 1_000, 0.0)];
    const b = [hop(1, "10.0.0.1", 1_000, 0.04)];
    const diff = diffRouteSnapshots(a, b);
    expect(diff.changed).toEqual([]);
    expect(diff.unchanged).toHaveLength(1);
  });

  test("returns empty unchanged when both snapshots are empty", () => {
    const diff = diffRouteSnapshots([], []);
    expect(diff.added).toEqual([]);
    expect(diff.removed).toEqual([]);
    expect(diff.changed).toEqual([]);
    expect(diff.unchanged).toEqual([]);
  });
});
