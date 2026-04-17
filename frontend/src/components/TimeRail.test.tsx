import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { RouteSnapshotSummary } from "@/api/hooks/nearby-snapshots";
import { TimeRail } from "./TimeRail";

function sum(id: number, observed_at: string): RouteSnapshotSummary {
  return { id, source_id: "a", target_id: "b", protocol: "icmp", observed_at };
}

const SAMPLE: RouteSnapshotSummary[] = [
  sum(1, "2026-04-17T09:00:00Z"),
  sum(2, "2026-04-17T09:05:00Z"),
  sum(3, "2026-04-17T09:10:00Z"),
  sum(4, "2026-04-17T09:12:04Z"),
  sum(5, "2026-04-17T09:14:41Z"),
  sum(6, "2026-04-17T09:17:00Z"),
  sum(7, "2026-04-17T09:20:00Z"),
];

describe("TimeRail", () => {
  it("renders up to maxTicks ticks always including the selected one", () => {
    render(
      <TimeRail
        side="A"
        selectedId={4}
        selectedMs={Date.UTC(2026, 3, 17, 9, 12, 4)}
        snapshots={SAMPLE}
        otherMarkerMs={Date.UTC(2026, 3, 17, 9, 14, 41)}
        maxTicks={5}
        onTickClick={() => {}}
      />,
    );
    const ticks = screen.getAllByRole("button", { name: /utc|^\d{2}:\d{2}/ });
    expect(ticks.length).toBeLessThanOrEqual(5);
    const selected = screen.getByRole("button", { pressed: true });
    expect(selected).toBeInTheDocument();
  });

  it("marks ticks on the wrong side of otherMarkerMs as disabled", () => {
    render(
      <TimeRail
        side="A"
        selectedId={4}
        selectedMs={Date.UTC(2026, 3, 17, 9, 12, 4)}
        snapshots={SAMPLE}
        otherMarkerMs={Date.UTC(2026, 3, 17, 9, 14, 41)}
        maxTicks={7}
        onTickClick={() => {}}
      />,
    );
    const blocked = screen.getAllByRole("button", { name: /crosses/i });
    expect(blocked.length).toBeGreaterThanOrEqual(1);
    for (const b of blocked) expect(b).toBeDisabled();
  });

  it("fires onTickClick with the snapshot when a valid tick is clicked", async () => {
    const user = userEvent.setup();
    const onTickClick = vi.fn();
    render(
      <TimeRail
        side="A"
        selectedId={4}
        selectedMs={Date.UTC(2026, 3, 17, 9, 12, 4)}
        snapshots={SAMPLE}
        otherMarkerMs={Date.UTC(2026, 3, 17, 9, 14, 41)}
        maxTicks={7}
        onTickClick={onTickClick}
      />,
    );
    const tick = screen.getByRole("button", { name: /09:05/ });
    await user.click(tick);
    expect(onTickClick).toHaveBeenCalledWith(expect.objectContaining({ id: 2 }));
  });

  it("renders a day-boundary marker when snapshots span UTC midnight", () => {
    const CROSS: RouteSnapshotSummary[] = [
      sum(1, "2026-04-16T23:55:00Z"),
      sum(2, "2026-04-17T00:00:00Z"),
      sum(3, "2026-04-17T00:05:15Z"),
    ];
    render(
      <TimeRail
        side="A"
        selectedId={1}
        selectedMs={Date.UTC(2026, 3, 16, 23, 55, 0)}
        snapshots={CROSS}
        otherMarkerMs={Date.UTC(2026, 3, 17, 0, 5, 15)}
        maxTicks={7}
        onTickClick={() => {}}
      />,
    );
    expect(screen.getByText(/apr 17/i)).toBeInTheDocument();
  });
});
