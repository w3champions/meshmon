import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import type { HistoryMeasurement } from "@/api/hooks/history";
import { PairChart } from "./PairChart";

function measurement(over: Partial<HistoryMeasurement>): HistoryMeasurement {
  return {
    id: 1,
    source_agent_id: "src-a",
    destination_ip: "10.0.0.1",
    protocol: "icmp",
    kind: "campaign",
    measured_at: "2026-04-20T00:00:00.000Z",
    probe_count: 10,
    loss_ratio: 0.1,
    latency_avg_ms: 12,
    latency_min_ms: 10,
    latency_max_ms: 15,
    latency_p95_ms: 14,
    latency_stddev_ms: 1,
    mtr_captured_at: null,
    mtr_hops: null,
    ...over,
  };
}

describe("PairChart", () => {
  test("renders an empty-state status when there are no measurements", () => {
    render(<PairChart measurements={[]} />);
    expect(screen.getByRole("status")).toHaveTextContent(/no measurements in the selected window/i);
  });

  test("renders latency and loss chart containers when data is present", () => {
    // Assert on the aria-labelled wrappers rather than the inner SVG — jsdom
    // doesn't paint recharts' responsive layout so the inner <svg> is
    // unreliable, but the role=img wrappers are stable.
    render(
      <PairChart
        measurements={[
          measurement({ id: 1, measured_at: "2026-04-20T00:00:00.000Z" }),
          measurement({ id: 2, measured_at: "2026-04-20T01:00:00.000Z", protocol: "tcp" }),
        ]}
      />,
    );
    expect(screen.getByRole("img", { name: /latency over time/i })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: /packet loss over time/i })).toBeInTheDocument();
  });
});
