import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import type { components } from "@/api/schema.gen";
import { RouteDiffSummary } from "@/components/RouteDiffSummary";
import { computeRouteDiff } from "@/lib/route-diff";

type HopJson = components["schemas"]["HopJson"];

function hop(position: number, ip: string, rttUs: number): HopJson {
  return {
    position,
    observed_ips: [{ ip, freq: 1 }],
    avg_rtt_micros: rttUs,
    stddev_rtt_micros: 100,
    loss_pct: 0,
  };
}

describe("RouteDiffSummary", () => {
  test("shows totals + first changed hop", () => {
    const a = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.2", 2_000)];
    const b = [hop(1, "10.0.0.1", 1_000), hop(2, "10.0.0.3", 2_000)];
    render(<RouteDiffSummary diff={computeRouteDiff(a, b)} />);
    expect(screen.getByText(/2 hops/i)).toBeInTheDocument();
    expect(screen.getByText(/1 changed/i)).toBeInTheDocument();
    expect(screen.getByText(/first change at hop 2/i)).toBeInTheDocument();
  });

  test("handles identical routes", () => {
    const a = [hop(1, "10.0.0.1", 1_000)];
    render(<RouteDiffSummary diff={computeRouteDiff(a, a)} />);
    expect(screen.getByText(/no changes/i)).toBeInTheDocument();
  });
});
