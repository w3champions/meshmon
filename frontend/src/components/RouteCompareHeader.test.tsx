import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { NearbySnapshotsResult } from "@/api/hooks/nearby-snapshots";
import type { components } from "@/api/schema.gen";
import { RouteCompareHeader } from "./RouteCompareHeader";

type Detail = components["schemas"]["RouteSnapshotDetail"];

function detail(id: number, observed_at: string): Detail {
  return {
    id,
    source_id: "fra-01",
    target_id: "nyc-02",
    protocol: "tcp",
    observed_at,
    hops: [],
  };
}

function sum(id: number, observed_at: string) {
  return { id, source_id: "fra-01", target_id: "nyc-02", protocol: "tcp", observed_at };
}

const NEARBY: NearbySnapshotsResult = {
  snapshots: [
    sum(3, "2026-04-17T09:09:10Z"),
    sum(4, "2026-04-17T09:12:04Z"),
    sum(5, "2026-04-17T09:13:08Z"),
    sum(6, "2026-04-17T09:13:55Z"),
    sum(7, "2026-04-17T09:14:41Z"),
    sum(8, "2026-04-17T09:16:59Z"),
  ],
  halfWindowMs: 15 * 60 * 1_000,
  findClosest: (target) => {
    if (target < Date.UTC(2026, 3, 17, 9, 10)) return sum(3, "2026-04-17T09:09:10Z");
    if (target < Date.UTC(2026, 3, 17, 9, 13)) return sum(4, "2026-04-17T09:12:04Z");
    return sum(7, "2026-04-17T09:14:41Z");
  },
  getNeighbors: (id) => {
    const order = [3, 4, 5, 6, 7, 8];
    const i = order.indexOf(id);
    return {
      prev: i > 0 ? sum(order[i - 1], "2026-04-17T09:00:00Z") : undefined,
      next: i < order.length - 1 ? sum(order[i + 1], "2026-04-17T09:20:00Z") : undefined,
    };
  },
  isLoading: false,
  isError: false,
};

describe("RouteCompareHeader", () => {
  it("renders path, protocol badge, and both observed_at timestamps", () => {
    render(
      <RouteCompareHeader
        source="fra-01"
        target="nyc-02"
        aDetail={detail(4, "2026-04-17T09:12:04Z")}
        bDetail={detail(7, "2026-04-17T09:14:41Z")}
        nearby={NEARBY}
        onNavA={() => {}}
        onNavB={() => {}}
      />,
    );
    expect(screen.getByText(/fra-01/)).toBeInTheDocument();
    expect(screen.getByText(/nyc-02/)).toBeInTheDocument();
    expect(screen.getByText(/^TCP$/i)).toBeInTheDocument();
    expect(screen.getAllByText(/09:12(?::04)?/).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/09:14(?::41)?/).length).toBeGreaterThan(0);
  });

  it("renders the Δ A→B chip", () => {
    render(
      <RouteCompareHeader
        source="fra-01"
        target="nyc-02"
        aDetail={detail(4, "2026-04-17T09:12:04Z")}
        bDetail={detail(7, "2026-04-17T09:14:41Z")}
        nearby={NEARBY}
        onNavA={() => {}}
        onNavB={() => {}}
      />,
    );
    expect(screen.getByText(/2m 37s/i)).toBeInTheDocument();
  });

  it("calls onNavA when a TimeStepper arrow is clicked on the A card", async () => {
    const onNavA = vi.fn();
    const { default: userEvent } = await import("@testing-library/user-event");
    const user = userEvent.setup();
    render(
      <RouteCompareHeader
        source="fra-01"
        target="nyc-02"
        aDetail={detail(4, "2026-04-17T09:12:04Z")}
        bDetail={detail(7, "2026-04-17T09:14:41Z")}
        nearby={NEARBY}
        onNavA={onNavA}
        onNavB={() => {}}
      />,
    );
    const btns = screen.getAllByRole("button", { name: /earlier/i });
    await user.click(btns[0]);
    expect(onNavA).toHaveBeenCalled();
  });

  it("masks A.next when it would cross or equal B (no later step for A)", () => {
    // Neighbors where A.next === B → the stepper must render disabled on A's
    // "later" arrow and duplicate copies of the label across responsive tiers.
    const nearby: NearbySnapshotsResult = {
      ...NEARBY,
      getNeighbors: (id) => {
        if (id === 4)
          return { prev: sum(3, "2026-04-17T09:09:10Z"), next: sum(7, "2026-04-17T09:14:41Z") };
        if (id === 7)
          return { prev: sum(4, "2026-04-17T09:12:04Z"), next: sum(8, "2026-04-17T09:16:59Z") };
        return {};
      },
    };
    render(
      <RouteCompareHeader
        source="fra-01"
        target="nyc-02"
        aDetail={detail(4, "2026-04-17T09:12:04Z")}
        bDetail={detail(7, "2026-04-17T09:14:41Z")}
        nearby={nearby}
        onNavA={() => {}}
        onNavB={() => {}}
      />,
    );
    // A's "later" arrows must show the "no later" placeholder, not the time delta.
    expect(screen.getAllByText(/no later/).length).toBeGreaterThan(0);
  });

  it("drops a jump whose nearest snapshot lands on the wrong side of the other marker", async () => {
    const onNavA = vi.fn();
    // findClosest always returns a snapshot past B (id=8, observed 09:16:59)
    // — handleJumpA must drop the nav instead of crossing.
    const nearby: NearbySnapshotsResult = {
      ...NEARBY,
      findClosest: () => sum(8, "2026-04-17T09:16:59Z"),
    };
    const { default: userEvent } = await import("@testing-library/user-event");
    const user = userEvent.setup();
    render(
      <RouteCompareHeader
        source="fra-01"
        target="nyc-02"
        aDetail={detail(4, "2026-04-17T09:12:04Z")}
        bDetail={detail(7, "2026-04-17T09:14:41Z")}
        nearby={nearby}
        onNavA={onNavA}
        onNavB={() => {}}
      />,
    );
    // Open the A-card Jump popover (first visible trigger) and click -5m.
    const jumpBtns = screen.getAllByRole("button", { name: /^jump/i });
    await user.click(jumpBtns[0]);
    const minusFive = await screen.findByRole("button", { name: /^-5m$/ });
    await user.click(minusFive);
    expect(onNavA).not.toHaveBeenCalled();
  });
});
