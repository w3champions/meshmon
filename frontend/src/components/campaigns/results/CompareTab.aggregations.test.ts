/**
 * Unit tests for CompareTab pure aggregation helpers.
 * P5: client-side re-aggregation for edge_candidate mode.
 */
import { describe, expect, test } from "vitest";
import type { EvaluationEdgePairDetailDto } from "@/api/hooks/evaluation";
import {
  aggregateEdgeCandidates,
  mergeAggregateIntoCandidate,
} from "@/components/campaigns/results/CompareTab.aggregations";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeRow(
  candidate_ip: string,
  destination_agent_id: string,
  best_route_ms: number,
  opts: {
    is_unreachable?: boolean;
    qualifies_under_t?: boolean;
    best_route_kind?: EvaluationEdgePairDetailDto["best_route_kind"];
  } = {},
): EvaluationEdgePairDetailDto {
  return {
    candidate_ip,
    destination_agent_id,
    best_route_ms,
    best_route_loss_ratio: 0,
    best_route_stddev_ms: 1,
    best_route_kind: opts.best_route_kind ?? "direct",
    best_route_legs: [],
    best_route_intermediaries: [],
    qualifies_under_t: opts.qualifies_under_t ?? best_route_ms < 100,
    is_unreachable: opts.is_unreachable ?? false,
  };
}

// ---------------------------------------------------------------------------
// aggregateEdgeCandidates
// ---------------------------------------------------------------------------

describe("aggregateEdgeCandidates", () => {
  test("returns empty array when pickedAgentIds is empty", () => {
    const rows = [makeRow("10.0.0.1", "agent-a", 50)];
    const result = aggregateEdgeCandidates(rows, new Set());
    expect(result).toHaveLength(0);
  });

  test("filters out rows whose destination_agent_id is not in picked set", () => {
    const rows = [
      makeRow("10.0.0.1", "agent-a", 50),
      makeRow("10.0.0.1", "agent-b", 60),
    ];
    const result = aggregateEdgeCandidates(rows, new Set(["agent-a"]));
    expect(result).toHaveLength(1);
    expect(result[0]!.destination_ip).toBe("10.0.0.1");
    // Only agent-a row counted
    expect(result[0]!.total_picked).toBe(1);
  });

  test("coverage_count counts qualifying rows only", () => {
    const rows = [
      makeRow("10.0.0.1", "agent-a", 50, { qualifies_under_t: true }),
      makeRow("10.0.0.1", "agent-b", 200, { qualifies_under_t: false }),
      makeRow("10.0.0.1", "agent-c", 80, { qualifies_under_t: true }),
    ];
    const result = aggregateEdgeCandidates(rows, new Set(["agent-a", "agent-b", "agent-c"]));
    expect(result[0]!.coverage_count).toBe(2);
  });

  test("mean_ms_under_t is null when no rows qualify", () => {
    const rows = [
      makeRow("10.0.0.1", "agent-a", 200, { qualifies_under_t: false }),
    ];
    const result = aggregateEdgeCandidates(rows, new Set(["agent-a"]));
    expect(result[0]!.mean_ms_under_t).toBeNull();
  });

  test("mean_ms_under_t is mean of qualifying best_route_ms values", () => {
    const rows = [
      makeRow("10.0.0.1", "agent-a", 40, { qualifies_under_t: true }),
      makeRow("10.0.0.1", "agent-b", 60, { qualifies_under_t: true }),
    ];
    const result = aggregateEdgeCandidates(rows, new Set(["agent-a", "agent-b"]));
    expect(result[0]!.mean_ms_under_t).toBeCloseTo(50);
  });

  test("unreachable rows do not count toward coverage or route mix", () => {
    const rows = [
      makeRow("10.0.0.1", "agent-a", 50, { is_unreachable: true, qualifies_under_t: false }),
      makeRow("10.0.0.1", "agent-b", 60, { qualifies_under_t: true }),
    ];
    const result = aggregateEdgeCandidates(rows, new Set(["agent-a", "agent-b"]));
    expect(result[0]!.coverage_count).toBe(1);
    // Only agent-b (reachable) counted in route mix
    expect(result[0]!.direct_share).toBe(1);
  });

  test("route mix shares sum to 1 over reachable rows", () => {
    const rows = [
      makeRow("10.0.0.1", "agent-a", 50, { best_route_kind: "direct" }),
      makeRow("10.0.0.1", "agent-b", 60, { best_route_kind: "one_hop" }),
      makeRow("10.0.0.1", "agent-c", 80, { best_route_kind: "two_hop" }),
    ];
    const result = aggregateEdgeCandidates(rows, new Set(["agent-a", "agent-b", "agent-c"]));
    const agg = result[0]!;
    expect(agg.direct_share).toBeCloseTo(1 / 3);
    expect(agg.onehop_share).toBeCloseTo(1 / 3);
    expect(agg.twohop_share).toBeCloseTo(1 / 3);
    expect((agg.direct_share ?? 0) + (agg.onehop_share ?? 0) + (agg.twohop_share ?? 0)).toBeCloseTo(1);
  });

  test("route mix shares are null when all rows unreachable", () => {
    const rows = [
      makeRow("10.0.0.1", "agent-a", 50, { is_unreachable: true }),
    ];
    const result = aggregateEdgeCandidates(rows, new Set(["agent-a"]));
    expect(result[0]!.direct_share).toBeNull();
    expect(result[0]!.onehop_share).toBeNull();
    expect(result[0]!.twohop_share).toBeNull();
  });

  test("aggregates multiple candidates independently", () => {
    const rows = [
      makeRow("10.0.0.1", "agent-a", 50, { qualifies_under_t: true }),
      makeRow("10.0.0.2", "agent-a", 80, { qualifies_under_t: true }),
      makeRow("10.0.0.1", "agent-b", 200, { qualifies_under_t: false }),
      makeRow("10.0.0.2", "agent-b", 30, { qualifies_under_t: true }),
    ];
    const result = aggregateEdgeCandidates(rows, new Set(["agent-a", "agent-b"]));
    const agg1 = result.find((r) => r.destination_ip === "10.0.0.1")!;
    const agg2 = result.find((r) => r.destination_ip === "10.0.0.2")!;
    expect(agg1.coverage_count).toBe(1);
    expect(agg2.coverage_count).toBe(2);
    expect(agg2.mean_ms_under_t).toBeCloseTo(55);
  });

  test("total_picked counts all picked-agent rows (reachable or not)", () => {
    const rows = [
      makeRow("10.0.0.1", "agent-a", 50),
      makeRow("10.0.0.1", "agent-b", 60, { is_unreachable: true }),
      makeRow("10.0.0.1", "agent-c", 80),
    ];
    const result = aggregateEdgeCandidates(rows, new Set(["agent-a", "agent-b", "agent-c"]));
    expect(result[0]!.total_picked).toBe(3);
  });
});

// ---------------------------------------------------------------------------
// mergeAggregateIntoCandidate
// ---------------------------------------------------------------------------

describe("mergeAggregateIntoCandidate", () => {
  const baseline = {
    destination_ip: "10.0.0.1",
    display_name: "cand-1",
    city: "Berlin",
    country_code: "DE",
    asn: 12345,
    network_operator: "Test ISP",
    hostname: "test.host",
    is_mesh_member: false,
    pairs_improved: 5,
    pairs_total_considered: 10,
    avg_improvement_ms: 20,
    avg_loss_ratio: 0.01,
    composite_score: 100,
    coverage_count: 8,
    coverage_weighted_ping_ms: 55,
    destinations_total: 10,
    direct_share: 0.8,
    onehop_share: 0.2,
    twohop_share: 0,
    mean_ms_under_t: 45,
  };

  test("overwrites coverage fields with aggregate values", () => {
    const agg = {
      destination_ip: "10.0.0.1",
      coverage_count: 3,
      total_picked: 5,
      mean_ms_under_t: 50,
      direct_share: 0.6,
      onehop_share: 0.4,
      twohop_share: 0,
    };
    const merged = mergeAggregateIntoCandidate(baseline, agg);
    expect(merged.coverage_count).toBe(3);
    expect(merged.mean_ms_under_t).toBe(50);
    expect(merged.destinations_total).toBe(5);
    expect(merged.direct_share).toBeCloseTo(0.6);
  });

  test("sets coverage_weighted_ping_ms to null (deferred)", () => {
    const agg = {
      destination_ip: "10.0.0.1",
      coverage_count: 2,
      total_picked: 3,
      mean_ms_under_t: 60,
      direct_share: 1,
      onehop_share: 0,
      twohop_share: 0,
    };
    const merged = mergeAggregateIntoCandidate(baseline, agg);
    expect(merged.coverage_weighted_ping_ms).toBeNull();
  });

  test("preserves non-coverage baseline fields unchanged", () => {
    const agg = {
      destination_ip: "10.0.0.1",
      coverage_count: 2,
      total_picked: 3,
      mean_ms_under_t: 60,
      direct_share: 1,
      onehop_share: 0,
      twohop_share: 0,
    };
    const merged = mergeAggregateIntoCandidate(baseline, agg);
    expect(merged.city).toBe("Berlin");
    expect(merged.asn).toBe(12345);
    expect(merged.is_mesh_member).toBe(false);
  });
});
