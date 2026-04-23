import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { type ReactNode, useEffect } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import "@/test/cytoscape-mock";
import { IpHostnameProvider, useSeedHostnames } from "@/components/ip-hostname";
import PathDetail from "@/pages/PathDetail";

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const now = new Date();
const freshSnapshot = {
  id: 42,
  source_id: "a",
  target_id: "b",
  protocol: "icmp",
  observed_at: now.toISOString(),
  hops: [
    {
      position: 1,
      observed_ips: [{ ip: "10.0.0.1", freq: 1 }],
      avg_rtt_micros: 1_000,
      stddev_rtt_micros: 100,
      loss_pct: 0,
    },
  ],
};

const overview = (opts?: { stale?: boolean }) => ({
  source: {
    id: "a",
    display_name: "Agent A",
    ip: "1.1.1.1",
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: now.toISOString(),
  },
  target: {
    id: "b",
    display_name: "Agent B",
    ip: "2.2.2.2",
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: now.toISOString(),
  },
  primary_protocol: "icmp",
  latest_by_protocol: {
    icmp: {
      ...freshSnapshot,
      observed_at: opts?.stale
        ? new Date(now.getTime() - 45 * 60 * 1000).toISOString()
        : now.toISOString(),
    },
    udp: null,
    tcp: null,
  },
  recent_snapshots: [],
  recent_snapshots_truncated: false,
  metrics: {
    rtt_series: [[0, 185]],
    loss_series: [[0, 0.001]],
    rtt_current: 185,
    loss_current: 0.001,
  },
  window: {
    from: new Date(now.getTime() - 24 * 3600_000).toISOString(),
    to: now.toISOString(),
  },
  step: "1m",
});

function mockEndpoints(body: unknown): void {
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = typeof input === "string" ? input : (input as Request).url;
    if (url.includes("/api/paths/a/b/overview")) {
      return new Response(JSON.stringify(body), { status: 200 });
    }
    return new Response("nf", { status: 404 });
  });
}

function renderPage(initialUrl = "/paths/a/b?range=24h"): {
  rendered: ReturnType<typeof render>;
  // TanStack Router's return type carries deep route generics that are
  // hard to restate here; `unknown` plus a narrow cast at use sites keeps
  // the test helper agnostic of the routeTree shape.
  router: {
    state: { location: { search: Record<string, unknown> } };
    history: { push: (path: string) => void };
  };
} {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const rootRoute = createRootRoute({ component: Outlet });
  const testRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/paths/$source/$target",
    component: PathDetail,
    // Mirror the production router schema — z.string().datetime() rejects
    // empty strings, so the Fix C guard must stop navigation before the
    // parse throws, otherwise the edit is silently dropped.
    validateSearch: (s: Record<string, unknown>) => {
      const from = typeof s.from === "string" ? s.from : undefined;
      const to = typeof s.to === "string" ? s.to : undefined;
      if (from !== undefined && from !== "" && Number.isNaN(Date.parse(from))) {
        throw new Error("invalid from");
      }
      if (from === "") throw new Error("custom range rejects empty from");
      if (to === "") throw new Error("custom range rejects empty to");
      return {
        range: (s.range ?? "24h") as "1h" | "6h" | "24h" | "7d" | "30d" | "2y" | "custom",
        from,
        to,
        protocol:
          s.protocol === "icmp" || s.protocol === "udp" || s.protocol === "tcp"
            ? s.protocol
            : undefined,
      };
    },
  });
  const compareRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/paths/$source/$target/routes/compare",
    component: () => null,
  });
  const reportRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/reports/path",
    component: () => null,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([testRoute, compareRoute, reportRoute]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
  const rendered = render(
    <QueryClientProvider client={qc}>
      <IpHostnameProvider>
        <RouterProvider router={router} />
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
  return { rendered, router };
}

// ---------------------------------------------------------------------------
// Provider-warmth helper: seeds the IpHostnameProvider map before the page
// renders, so the test exercises the seed → first-paint hostname path without
// waiting for a SSE event.
// ---------------------------------------------------------------------------
interface SeedEntry {
  ip: string;
  hostname?: string | null;
}

function Seeder({ seed, children }: { seed: SeedEntry[]; children: ReactNode }) {
  const seedFromResponse = useSeedHostnames();
  // biome-ignore lint/correctness/useExhaustiveDependencies: mount-only seed
  useEffect(() => {
    if (seed.length > 0) seedFromResponse(seed);
  }, []);
  return <>{children}</>;
}

function renderPagePreSeeded(
  seed: SeedEntry[],
  initialUrl = "/paths/a/b?range=24h",
): ReturnType<typeof renderPage> {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const rootRoute = createRootRoute({ component: Outlet });
  const testRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/paths/$source/$target",
    component: PathDetail,
    validateSearch: (s: Record<string, unknown>) => {
      const from = typeof s.from === "string" ? s.from : undefined;
      const to = typeof s.to === "string" ? s.to : undefined;
      return {
        range: (s.range ?? "24h") as "1h" | "6h" | "24h" | "7d" | "30d" | "2y" | "custom",
        from,
        to,
        protocol:
          s.protocol === "icmp" || s.protocol === "udp" || s.protocol === "tcp"
            ? s.protocol
            : undefined,
      };
    },
  });
  const compareRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/paths/$source/$target/routes/compare",
    component: () => null,
  });
  const reportRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/reports/path",
    component: () => null,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([testRoute, compareRoute, reportRoute]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
  const rendered = render(
    <QueryClientProvider client={qc}>
      <IpHostnameProvider>
        <Seeder seed={seed}>
          <RouterProvider router={router} />
        </Seeder>
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
  return {
    rendered,
    router: router as ReturnType<typeof renderPage>["router"],
  };
}

describe("PathDetail", () => {
  test("renders source + target cards, Grafana iframes, sparklines", async () => {
    mockEndpoints(overview());
    renderPage();
    expect(await screen.findByText("Agent A")).toBeInTheDocument();
    expect(await screen.findByText("Agent B")).toBeInTheDocument();
    expect(await screen.findByTitle("RTT")).toBeInTheDocument();
    expect(await screen.findByTitle("Loss")).toBeInTheDocument();
    expect(await screen.findByTitle("Stddev")).toBeInTheDocument();
    expect(await screen.findByLabelText("RTT trend")).toBeInTheDocument();
    expect(await screen.findByLabelText("Loss trend")).toBeInTheDocument();
  });

  test("shows stale banner when the latest snapshot is > 30 min old", async () => {
    mockEndpoints(overview({ stale: true }));
    renderPage();
    expect(await screen.findByText(/data may be stale/i)).toBeInTheDocument();
  });

  test("'Generate report' link carries the expected query", async () => {
    mockEndpoints(overview());
    renderPage();
    const link = await screen.findByRole("link", { name: /generate report/i });
    const href = link.getAttribute("href") ?? "";
    expect(href).toContain("/reports/path");
    // Agent IDs, not IPs — the report URL is keyed on stable agent IDs so
    // shared links don't break when an agent's IP changes.
    expect(href).toContain("source_id=a");
    expect(href).toContain("target_id=b");
    expect(href).toContain("protocol=icmp");
    expect(href).not.toContain("source_ip=");
    expect(href).not.toContain("target_ip=");
  });

  test("renders the Hostname column header in the current route hops table", async () => {
    mockEndpoints(overview());
    renderPage();
    // The "Current route hops" section is rendered when latest has hops.
    // freshSnapshot has one hop, so the RouteTable is mounted and should show
    // a Hostname column header.
    expect(await screen.findByRole("columnheader", { name: /hostname/i })).toBeInTheDocument();
  });

  test("renders hop IP in the Hostname column (cold miss → bare IP)", async () => {
    mockEndpoints(overview());
    renderPage();
    // The hop IP "10.0.0.1" is the dominant IP from freshSnapshot. IpHostname
    // renders it as the bare IP on a cold miss (no provider seeding yet).
    expect(await screen.findByText("10.0.0.1")).toBeInTheDocument();
  });

  test("renders hostname alongside hop IP when provider map is pre-seeded (warmth)", async () => {
    // Provider-warmth test: verifies the seed → first-paint pipeline.
    // The hop IP "10.0.0.1" is seeded with a hostname before the page mounts,
    // so IpHostname should display it without waiting for a SSE event.
    mockEndpoints(overview());
    renderPagePreSeeded([{ ip: "10.0.0.1", hostname: "router.example.com" }]);
    // IpHostname renders "ip (hostname)" as accessible text: the visible text is
    // "10.0.0.1" with a muted "(router.example.com)" suffix. The accessible name
    // combines them, so checking for the hostname string is sufficient.
    expect(await screen.findByText("(router.example.com)")).toBeInTheDocument();
  });

  test("renders empty-state when no snapshots exist in window", async () => {
    mockEndpoints({
      ...overview(),
      primary_protocol: null,
      latest_by_protocol: { icmp: null, udp: null, tcp: null },
    });
    renderPage();
    // "Primary protocol: —" instead of a fabricated ICMP label.
    const label = await screen.findByText(/primary protocol:/i);
    expect(label.textContent).toMatch(/—/);
    // Report link is hidden (protocol-specific, and we have no protocol).
    expect(screen.queryByRole("link", { name: /generate report/i })).toBeNull();
  });

  test("keeps previous data visible across protocol change (no full-page skeleton flash)", async () => {
    // Simulate a protocol toggle by navigating to the same page with a
    // different `protocol` query param. The query key changes, so TanStack
    // Query refetches — but with `placeholderData: keepPreviousData` the old
    // data remains rendered while the new fetch is in flight, and PathDetail
    // must NOT fall back to its top-level skeleton.
    let resolveSecondFetch: ((body: unknown) => void) | null = null;
    let callCount = 0;
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = typeof input === "string" ? input : (input as Request).url;
      if (url.includes("/api/paths/a/b/overview")) {
        callCount += 1;
        if (callCount === 1) {
          return new Response(JSON.stringify(overview()), { status: 200 });
        }
        // Hold the second fetch open so we can observe the "refetch in flight
        // with previous data visible" state.
        const body = await new Promise<unknown>((resolve) => {
          resolveSecondFetch = resolve;
        });
        return new Response(JSON.stringify(body), { status: 200 });
      }
      return new Response("nf", { status: 404 });
    });

    const { router } = renderPage("/paths/a/b?range=24h&protocol=icmp");
    // First render finishes with real content on screen.
    expect(await screen.findByText("Agent A")).toBeInTheDocument();
    expect(screen.queryByTestId("path-detail-skeleton")).toBeNull();

    // Navigate to a different protocol to change the TanStack Query key.
    // Going through history.push (a plain string) avoids TanStack Router's
    // deeply-generic `navigate()` signature, which is a nightmare to satisfy
    // from test helpers.
    await act(async () => {
      router.history.push("/paths/a/b?range=24h&protocol=udp");
    });

    // While the second fetch is pending, the previous data is still on screen
    // and the page skeleton must NOT come back — that was the visual "full
    // page refresh" the user complained about.
    expect(screen.queryByTestId("path-detail-skeleton")).toBeNull();
    expect(screen.getByText("Agent A")).toBeInTheDocument();
    expect(await screen.findByText(/refreshing/i)).toBeInTheDocument();

    // Let the second fetch complete; the page simply swaps in fresh data
    // without ever showing the skeleton.
    await act(async () => {
      resolveSecondFetch?.(overview());
    });
    await waitFor(() => {
      expect(screen.queryByText(/refreshing/i)).toBeNull();
    });
    expect(screen.queryByTestId("path-detail-skeleton")).toBeNull();
  });

  test("drops intermediate custom-range edits with empty from/to", async () => {
    mockEndpoints(overview());
    // Start in custom mode with both bounds populated — the realistic
    // precondition when the user has already opened the date pickers.
    const initialFrom = "2026-04-13T10:00:00.000Z";
    const initialTo = "2026-04-13T14:00:00.000Z";
    const initialUrl = `/paths/a/b?range=custom&from=${encodeURIComponent(
      initialFrom,
    )}&to=${encodeURIComponent(initialTo)}`;
    const { router } = renderPage(initialUrl);

    // Wait for the page to finish first render so CustomRangeInputs exist.
    await screen.findByText("Agent A");

    // Clearing the `From` input emits from="" while the user types/edits —
    // the router schema rejects empty strings, so the component must skip
    // navigation rather than throw and silently lose subsequent edits.
    const fromInput = screen.getByLabelText(/from/i);
    fireEvent.change(fromInput, { target: { value: "" } });

    // URL stays on the original custom range — the empty intermediate
    // state was dropped instead of replacing the valid bounds.
    const search = router.state.location.search as Record<string, unknown>;
    expect(search.range).toBe("custom");
    expect(search.from).toBe(initialFrom);
    expect(search.to).toBe(initialTo);
  });
});
