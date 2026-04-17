import "@testing-library/jest-dom/vitest";
import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { components } from "@/api/schema.gen";
import { RouteTable } from "./RouteTable";

type HopJson = components["schemas"]["HopJson"];

function hop(
  position: number,
  ip: string,
  rtt_us: number,
  loss_pct: number,
  freq = 1,
): HopJson {
  return {
    position,
    avg_rtt_micros: rtt_us,
    loss_pct,
    observed_ips: [{ ip, freq }],
    stddev_rtt_micros: 0,
  };
}

describe("RouteTable", () => {
  it("renders one row per hop with formatted values", () => {
    render(
      <RouteTable
        hops={[
          hop(1, "10.0.0.1", 1_200, 0),
          hop(2, "10.0.0.2", 50_000, 0.02, 0.75),
        ]}
      />,
    );
    const rows = screen.getAllByRole("row");
    expect(rows).toHaveLength(3); // 1 header + 2 body

    const r1 = within(rows[1]);
    expect(r1.getByText("1")).toBeInTheDocument();
    expect(r1.getByText("10.0.0.1")).toBeInTheDocument();
    expect(r1.getByText(/1\.2\s?ms/)).toBeInTheDocument();
    expect(r1.getByText("0.00%")).toBeInTheDocument();

    const r2 = within(rows[2]);
    expect(r2.getByText("75%")).toBeInTheDocument();
    expect(r2.getByText("2.00%")).toBeInTheDocument();
  });

  it("renders an empty-state row when hops is empty", () => {
    render(<RouteTable hops={[]} />);
    expect(screen.getByText(/no hops recorded/i)).toBeInTheDocument();
  });

  it("highlights changed rows when a diff is provided", () => {
    const hops = [
      hop(1, "10.0.0.1", 1_000, 0),
      hop(2, "10.0.0.9", 2_000, 0),
    ];
    render(
      <RouteTable
        hops={hops}
        diff={{
          changedPositions: new Set([2]),
          addedPositions: new Set<number>(),
          removedPositions: new Set<number>(),
        }}
      />,
    );
    const rows = screen.getAllByRole("row");
    expect(rows[2]).toHaveAttribute("data-diff-state", "changed");
    expect(within(rows[2]).getByText(/★ changed/i)).toBeInTheDocument();
  });

  it("marks added rows with data-diff-state=added", () => {
    const hops = [hop(2, "10.0.0.5", 1_000, 0)];
    render(
      <RouteTable
        hops={hops}
        diff={{
          changedPositions: new Set<number>(),
          addedPositions: new Set([2]),
          removedPositions: new Set<number>(),
        }}
      />,
    );
    const rows = screen.getAllByRole("row");
    expect(rows[1]).toHaveAttribute("data-diff-state", "added");
    expect(within(rows[1]).getByText(/\+ added/i)).toBeInTheDocument();
  });
});
