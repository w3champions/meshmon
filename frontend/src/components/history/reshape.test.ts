import { describe, expect, test } from "vitest";
import type { HistoryMeasurement } from "@/api/hooks/history";
import { CHART_PROTOCOLS, protocolsPresent, reshapeForChart } from "./reshape";

function measurement(over: Partial<HistoryMeasurement>): HistoryMeasurement {
  return {
    id: 1,
    source_agent_id: "src-a",
    destination_ip: "10.0.0.1",
    protocol: "icmp",
    kind: "campaign",
    measured_at: "2026-04-20T00:00:00.000Z",
    probe_count: 10,
    loss_pct: 0,
    latency_avg_ms: null,
    latency_min_ms: null,
    latency_max_ms: null,
    latency_p95_ms: null,
    latency_stddev_ms: null,
    mtr_captured_at: null,
    mtr_hops: null,
    ...over,
  };
}

describe("reshapeForChart", () => {
  test("returns empty array for empty input", () => {
    expect(reshapeForChart([])).toEqual([]);
  });

  test("emits one row per measured_at bucket with flat protocol keys", () => {
    const rows = reshapeForChart([
      measurement({
        id: 1,
        protocol: "icmp",
        measured_at: "2026-04-20T00:00:00.000Z",
        latency_avg_ms: 12,
        latency_min_ms: 10,
        latency_max_ms: 15,
        loss_pct: 0.5,
      }),
      measurement({
        id: 2,
        protocol: "tcp",
        measured_at: "2026-04-20T00:00:00.000Z",
        latency_avg_ms: 30,
        latency_min_ms: 20,
        latency_max_ms: 40,
        loss_pct: 1.0,
      }),
    ]);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      t: "2026-04-20T00:00:00.000Z",
      icmp_avg: 12,
      icmp_min: 10,
      icmp_max: 15,
      icmp_range_delta: 5,
      icmp_loss: 0.5,
      tcp_avg: 30,
      tcp_min: 20,
      tcp_max: 40,
      tcp_range_delta: 20,
      tcp_loss: 1.0,
    });
  });

  test("sorts output by timestamp ascending", () => {
    const rows = reshapeForChart([
      measurement({ id: 1, measured_at: "2026-04-20T02:00:00.000Z" }),
      measurement({ id: 2, measured_at: "2026-04-20T00:00:00.000Z" }),
      measurement({ id: 3, measured_at: "2026-04-20T01:00:00.000Z" }),
    ]);
    expect(rows.map((r) => r.t)).toEqual([
      "2026-04-20T00:00:00.000Z",
      "2026-04-20T01:00:00.000Z",
      "2026-04-20T02:00:00.000Z",
    ]);
  });

  test("omits avg/min/max keys when the source field is null", () => {
    const rows = reshapeForChart([
      measurement({
        protocol: "udp",
        measured_at: "2026-04-20T00:00:00.000Z",
        latency_avg_ms: null,
        latency_min_ms: null,
        latency_max_ms: null,
        loss_pct: 2.5,
      }),
    ]);
    expect(rows[0].udp_avg).toBeUndefined();
    expect(rows[0].udp_min).toBeUndefined();
    expect(rows[0].udp_max).toBeUndefined();
    expect(rows[0].udp_range_delta).toBeUndefined();
    // loss_pct is non-nullable so it always lands in the row
    expect(rows[0].udp_loss).toBe(2.5);
  });

  test("only emits range_delta when both min and max are present", () => {
    const rows = reshapeForChart([
      measurement({
        id: 1,
        protocol: "icmp",
        measured_at: "2026-04-20T00:00:00.000Z",
        latency_min_ms: 10,
        latency_max_ms: null,
      }),
      measurement({
        id: 2,
        protocol: "tcp",
        measured_at: "2026-04-20T00:00:00.000Z",
        latency_min_ms: 5,
        latency_max_ms: 25,
      }),
    ]);
    expect(rows[0].icmp_range_delta).toBeUndefined();
    expect(rows[0].tcp_range_delta).toBe(20);
  });

  test("later writes win when two measurements share (bucket, protocol)", () => {
    const rows = reshapeForChart([
      measurement({
        id: 1,
        protocol: "icmp",
        measured_at: "2026-04-20T00:00:00.000Z",
        latency_avg_ms: 10,
        loss_pct: 0,
      }),
      measurement({
        id: 2,
        protocol: "icmp",
        measured_at: "2026-04-20T00:00:00.000Z",
        latency_avg_ms: 99,
        loss_pct: 3,
      }),
    ]);
    expect(rows).toHaveLength(1);
    expect(rows[0].icmp_avg).toBe(99);
    expect(rows[0].icmp_loss).toBe(3);
  });
});

describe("protocolsPresent", () => {
  test("returns the protocols actually carried by the dataset", () => {
    const rows = reshapeForChart([
      measurement({ protocol: "icmp", latency_avg_ms: 5 }),
      measurement({
        id: 2,
        protocol: "tcp",
        measured_at: "2026-04-20T01:00:00.000Z",
        latency_avg_ms: 10,
      }),
    ]);
    expect(protocolsPresent(rows)).toEqual(["icmp", "tcp"]);
  });

  test("includes protocols even when only loss is present", () => {
    const rows = reshapeForChart([
      measurement({
        protocol: "udp",
        latency_avg_ms: null,
        loss_pct: 4.2,
      }),
    ]);
    expect(protocolsPresent(rows)).toEqual(["udp"]);
  });

  test("returns empty when rows carry no protocol-scoped data", () => {
    expect(protocolsPresent([])).toEqual([]);
  });

  test("preserves CHART_PROTOCOLS order regardless of input order", () => {
    const rows = reshapeForChart([
      measurement({ protocol: "udp", latency_avg_ms: 5 }),
      measurement({
        id: 2,
        protocol: "icmp",
        measured_at: "2026-04-20T01:00:00.000Z",
        latency_avg_ms: 10,
      }),
    ]);
    expect(protocolsPresent(rows)).toEqual(["icmp", "udp"]);
    // And the exported CHART_PROTOCOLS constant is the source of truth.
    expect(CHART_PROTOCOLS).toEqual(["icmp", "tcp", "udp"]);
  });
});
