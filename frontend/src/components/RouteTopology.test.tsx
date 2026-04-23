import "@testing-library/jest-dom/vitest";
// Mock cytoscape BEFORE importing RouteTopology so the component picks up the
// stub instead of the real library (which needs a browser layout engine).
import "@/test/cytoscape-mock";
import { act, cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { components } from "@/api/schema.gen";
import {
  type HostnameSeedEntry,
  IpHostnameProvider,
  useSeedHostnames,
} from "@/components/ip-hostname";
import { RouteTopology, truncateHostname } from "@/components/RouteTopology";
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

const THREE_HOPS: HopJson[] = [
  {
    position: 1,
    observed_ips: [{ ip: "10.0.0.1", freq: 1 }],
    avg_rtt_micros: 1_000,
    stddev_rtt_micros: 100,
    loss_pct: 0,
  },
  {
    position: 2,
    observed_ips: [{ ip: "10.0.0.2", freq: 1 }],
    avg_rtt_micros: 2_000,
    stddev_rtt_micros: 200,
    loss_pct: 0,
  },
  {
    position: 3,
    observed_ips: [{ ip: "10.0.0.3", freq: 1 }],
    avg_rtt_micros: 3_000,
    stddev_rtt_micros: 300,
    loss_pct: 0,
  },
];

beforeEach(() => {
  instances.length = 0;
});

afterEach(cleanup);

describe("RouteTopology", () => {
  test("renders one node per hop and n-1 edges", () => {
    render(
      <IpHostnameProvider>
        <RouteTopology hops={HOPS} />
      </IpHostnameProvider>,
    );
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
    const { unmount } = render(
      <IpHostnameProvider>
        <RouteTopology hops={HOPS} />
      </IpHostnameProvider>,
    );
    unmount();
    expect(instances[0].destroyed).toBe(true);
  });

  test("fires onNodeClick with the hop when a node is tapped", () => {
    const onNodeClick = vi.fn();
    render(
      <IpHostnameProvider>
        <RouteTopology hops={HOPS} onNodeClick={onNodeClick} />
      </IpHostnameProvider>,
    );
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
    render(
      <IpHostnameProvider>
        <RouteTopology hops={other} highlightChanges={diff.perHop} />
      </IpHostnameProvider>,
    );
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
    const { getByText } = render(
      <IpHostnameProvider>
        <RouteTopology hops={[]} />
      </IpHostnameProvider>,
    );
    expect(getByText(/no route data/i)).toBeInTheDocument();
    expect(instances).toHaveLength(0);
  });

  test("sr-only description reflects hops for screen readers", () => {
    const { container } = render(
      <IpHostnameProvider>
        <RouteTopology hops={HOPS} ariaLabel="Route" />
      </IpHostnameProvider>,
    );
    const sr = container.querySelector(".sr-only");
    expect(sr?.textContent).toContain("10.0.0.1");
    expect(sr?.textContent).toContain("10.0.0.2");
  });
});

describe("RouteTopology accessibility", () => {
  test("does not render a visible 'Route hops' caption", () => {
    render(
      <IpHostnameProvider>
        <RouteTopology hops={HOPS} ariaLabel="x" />
      </IpHostnameProvider>,
    );
    expect(screen.queryByText("Route hops", { selector: "caption" })).toBeNull();
  });

  test("exposes hop list via aria-describedby on the graph", () => {
    render(
      <IpHostnameProvider>
        <RouteTopology hops={[HOPS[0]]} ariaLabel="topology" />
      </IpHostnameProvider>,
    );
    const graph = screen.getByRole("img", { name: "topology" });
    const describedBy = graph.getAttribute("aria-describedby");
    expect(describedBy).toBeTruthy();
    if (!describedBy) throw new Error("unreachable");
    const desc = document.getElementById(describedBy);
    expect(desc).not.toBeNull();
    expect(desc?.textContent ?? "").toContain("10.0.0.1");
  });
});

// ---------------------------------------------------------------------------
// T53c I4 — in-place hostname label update tests
// ---------------------------------------------------------------------------

/**
 * Helper wrapper that exposes the provider's seed callback to tests via
 * the sanctioned `useSeedHostnames()` barrel export — no direct
 * `useIpHostnameContext` import, so tests honour the
 * `components/ip-hostname/` public-surface contract.
 */
function makeWrapper() {
  let seedRef: ((entries: Iterable<HostnameSeedEntry>) => void) | null = null;

  function Capture({ children }: { children: ReactNode }) {
    seedRef = useSeedHostnames();
    return <>{children}</>;
  }

  function Wrapper({ children }: { children: ReactNode }) {
    return (
      <IpHostnameProvider>
        <Capture>{children}</Capture>
      </IpHostnameProvider>
    );
  }

  return {
    Wrapper,
    seed(entries: Iterable<HostnameSeedEntry>) {
      if (!seedRef) throw new Error("Wrapper not yet mounted");
      seedRef(entries);
    },
  };
}

describe("RouteTopology in-place hostname label updates (T53c I4)", () => {
  test("updates node label in-place when hostname arrives — no new Cy instance, no re-layout", async () => {
    const { Wrapper, seed } = makeWrapper();

    render(
      <Wrapper>
        <RouteTopology hops={THREE_HOPS} />
      </Wrapper>,
    );

    // Initially no hostnames — label is just position + IP.
    expect(instances).toHaveLength(1);
    const cy = instances[0];
    expect(cy.nodeData.get("1")?.label).toBe("#1\n10.0.0.1");

    // Seed a hostname for the first hop's IP.
    act(() => {
      seed([{ ip: "10.0.0.1", hostname: "foo.example.com" }]);
    });

    // Still exactly one Cy instance (no teardown/rebuild).
    expect(instances).toHaveLength(1);
    expect(cy.destroyed).toBe(false);

    // Label updated in place via getElementById.data().
    expect(cy.nodeData.get("1")?.label).toBe("#1\n10.0.0.1\nfoo.example.com");

    // No re-layout triggered (layoutCalls stays at the initial 1 from mount).
    expect(cy.layoutCalls).toBe(1);
  });

  test("initial label has no hostname when provider has no data yet", () => {
    const { Wrapper } = makeWrapper();

    render(
      <Wrapper>
        <RouteTopology hops={THREE_HOPS} />
      </Wrapper>,
    );

    expect(instances[0].nodeData.get("1")?.label).toBe("#1\n10.0.0.1");
  });

  test("truncates long hostnames in the Cytoscape node label", () => {
    const { Wrapper, seed } = makeWrapper();

    render(
      <Wrapper>
        <RouteTopology hops={THREE_HOPS} />
      </Wrapper>,
    );

    const longHostname = "very-long-subdomain.internal.infrastructure.example.com";
    act(() => {
      seed([{ ip: "10.0.0.1", hostname: longHostname }]);
    });

    const label = instances[0].nodeData.get("1")?.label as string;
    // Label must contain truncated form (middle ellipsis), not the full hostname.
    expect(label).toContain("…");
    expect(label).not.toContain(longHostname);
    // Truncated form: first 10 + … + last 10 chars.
    // "very-long-subdomain.internal.infrastructure.example.com" → "very-long-…xample.com"
    expect(label).toContain("very-long-");
    expect(label).toContain("xample.com");
  });
});

// ---------------------------------------------------------------------------
// truncateHostname unit tests
// ---------------------------------------------------------------------------

describe("truncateHostname", () => {
  test("returns hostname unchanged when ≤ 24 chars", () => {
    expect(truncateHostname("short.host.example")).toBe("short.host.example");
    expect(truncateHostname("a".repeat(24))).toBe("a".repeat(24));
  });

  test("middle-truncates hostnames longer than 24 chars", () => {
    const long = "very-long-subdomain.internal.infrastructure.example.com";
    const result = truncateHostname(long);
    expect(result).toContain("…");
    expect(result).toBe(`${long.slice(0, 10)}…${long.slice(-10)}`);
    // Total visible chars: 10 + ellipsis + 10 = 21 chars (not 24 limit)
    expect(result.replace("…", "").length).toBe(20);
  });

  test("handles exactly 25 chars (one over the limit)", () => {
    const host = "a".repeat(25);
    const result = truncateHostname(host);
    expect(result).toBe(`${"a".repeat(10)}…${"a".repeat(10)}`);
  });

  test("never produces a lone surrogate when a non-BMP code point straddles the cut boundary", () => {
    // "😀" (U+1F600) is non-BMP — UTF-16 pair 0xD83D 0xDE00. Naive
    // `slice` by UTF-16 code units would put the high surrogate in the
    // head and the low surrogate in the tail.
    // Place an emoji exactly at the cut boundary: if the function sliced
    // by code unit (length 26 in UTF-16 code units), `slice(0, 10)` and
    // `slice(-10)` would each clip across a surrogate pair.
    // 9 'a's + 😀 + 15 'b's → 25 code points, boundary inside the emoji.
    const input = `${"a".repeat(9)}😀${"b".repeat(15)}`;
    const result = truncateHostname(input);
    expect(result).toContain("…");
    // Manually scan for lone surrogates: a high surrogate (0xD800-0xDBFF)
    // must be immediately followed by a low surrogate (0xDC00-0xDFFF),
    // and no low surrogate may appear without a preceding high surrogate.
    for (let i = 0; i < result.length; i += 1) {
      const code = result.charCodeAt(i);
      const isHigh = code >= 0xd800 && code <= 0xdbff;
      const isLow = code >= 0xdc00 && code <= 0xdfff;
      if (isHigh) {
        const next = result.charCodeAt(i + 1);
        expect(next >= 0xdc00 && next <= 0xdfff).toBe(true);
        i += 1; // skip the low surrogate we just validated
      } else {
        expect(isLow).toBe(false);
      }
    }
    // First 10 code points: 9 'a's + 😀 (kept whole).
    expect(result.startsWith(`${"a".repeat(9)}😀`)).toBe(true);
  });
});
