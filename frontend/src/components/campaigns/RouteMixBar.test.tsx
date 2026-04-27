// RouteMixBar.test.tsx
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { RouteMixBar } from "./RouteMixBar";

describe("RouteMixBar", () => {
  it("renders three segments with correct widths", () => {
    const { container } = render(<RouteMixBar direct={0.5} oneHop={0.3} twoHop={0.2} />);
    const segments = container.querySelectorAll("[data-segment]");
    expect(segments).toHaveLength(3);
    expect((segments[0] as HTMLElement).style.width).toBe("50%");
  });

  it("emits aria-label with percentage breakdown", () => {
    const { getByRole } = render(<RouteMixBar direct={0.6} oneHop={0.3} twoHop={0.1} />);
    const bar = getByRole("img");
    expect(bar.getAttribute("aria-label")).toContain("60% direct");
    expect(bar.getAttribute("aria-label")).toContain("30% one-hop");
    expect(bar.getAttribute("aria-label")).toContain("10% two-hop");
  });

  it("handles edge cases — all zero shares", () => {
    const { getByRole } = render(<RouteMixBar direct={0} oneHop={0} twoHop={0} />);
    expect(getByRole("img").getAttribute("aria-label")).toContain("no reachable destinations");
  });
});
