import "@testing-library/jest-dom/vitest";
// Mock cytoscape BEFORE importing RouteTopology so the component picks up the
// stub instead of the real library (which needs a browser layout engine).
import "@/test/cytoscape-mock";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { components } from "@/api/schema.gen";
import { RouteTopology } from "@/components/RouteTopology";
import { computeRouteDiff } from "@/lib/route-diff";
import { instances } from "@/test/cytoscape-mock";

type HopJson = components["schemas"]["HopJson"];

const HOPS: HopJson[] = [
  {
    position: 1,
    observed_ips: [{ ip: "10.0.0.1", freq: 1 }],
    avg_rtt_micros: 1_000,
    stddev_rtt_micros: 100,
    loss_pct: 0,
  },
  {
    position: 2,
    observed_ips: [
      { ip: "10.0.0.2", freq: 7 },
      { ip: "10.0.0.3", freq: 3 },
    ],
    avg_rtt_micros: 2_000,
    stddev_rtt_micros: 200,
    loss_pct: 0.1,
  },
];

beforeEach(() => {
  instances.length = 0;
});

afterEach(cleanup);

describe("RouteTopology", () => {
  test("renders one node per hop and n-1 edges", () => {
    render(<RouteTopology hops={HOPS} />);
    expect(instances).toHaveLength(1);
    const cy = instances[0];
    const nodes = cy.elements.filter(
      (e): e is { data: { id: string; label: string } } =>
        typeof e === "object" &&
        e !== null &&
        "data" in e &&
        typeof (e as { data: unknown }).data === "object" &&
        (e as { data: Record<string, unknown> }).data !== null &&
        "id" in (e as { data: Record<string, unknown> }).data &&
        !("source" in (e as { data: Record<string, unknown> }).data),
    );
    const edges = cy.elements.filter(
      (e): e is { data: { id: string; source: string; target: string } } =>
        typeof e === "object" &&
        e !== null &&
        "data" in e &&
        typeof (e as { data: unknown }).data === "object" &&
        (e as { data: Record<string, unknown> }).data !== null &&
        "source" in (e as { data: Record<string, unknown> }).data,
    );
    expect(nodes).toHaveLength(2);
    expect(edges).toHaveLength(1);
    expect(nodes[1].data.label).toContain("10.0.0.2");
  });

  test("destroys the cytoscape instance on unmount", () => {
    const { unmount } = render(<RouteTopology hops={HOPS} />);
    unmount();
    expect(instances[0].destroyed).toBe(true);
  });

  test("fires onNodeClick with the hop when a node is tapped", () => {
    const onNodeClick = vi.fn();
    render(<RouteTopology hops={HOPS} onNodeClick={onNodeClick} />);
    act(() => {
      instances[0].handlers.tap?.({ target: { id: () => "2" } });
    });
    expect(onNodeClick).toHaveBeenCalledWith(HOPS[1]);
  });

  test("applies diff-highlight classes when highlightChanges is passed", () => {
    const other: HopJson[] = [
      HOPS[0],
      { ...HOPS[1], observed_ips: [{ ip: "99.99.99.99", freq: 1 }] },
    ];
    const diff = computeRouteDiff(HOPS, other);
    render(<RouteTopology hops={other} highlightChanges={diff.perHop} />);
    const node2 = instances[0].elements.find(
      (e): e is { data: { id: string }; classes: string } => {
        if (typeof e !== "object" || e === null) return false;
        const rec = e as { data?: Record<string, unknown>; classes?: unknown };
        return (
          rec.data?.id === "2" && !("source" in (rec.data ?? {})) && typeof rec.classes === "string"
        );
      },
    );
    expect(node2?.classes).toMatch(/diff-changed/);
  });

  test("renders placeholder when hops is empty", () => {
    const { getByText } = render(<RouteTopology hops={[]} />);
    expect(getByText(/no route data/i)).toBeInTheDocument();
    expect(instances).toHaveLength(0);
  });

  test("sr-only table reflects hops for screen readers", () => {
    const { container } = render(<RouteTopology hops={HOPS} ariaLabel="Route" />);
    const sr = container.querySelector(".sr-only");
    expect(sr?.textContent).toContain("10.0.0.1");
    expect(sr?.textContent).toContain("10.0.0.2");
  });
});
