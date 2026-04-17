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
import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import "@/test/cytoscape-mock";
import PathDetail from "@/pages/PathDetail";

afterEach(() => vi.restoreAllMocks());

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
    if (url.includes("/api/web-config")) {
      return new Response(
        JSON.stringify({
          version: "0.1.0",
          username: "u",
          grafana_base_url: "https://grafana.example/",
          grafana_dashboards: { "meshmon-path": "abc" },
        }),
        { status: 200 },
      );
    }
    if (url.includes("/api/paths/a/b/overview")) {
      return new Response(JSON.stringify(body), { status: 200 });
    }
    return new Response("nf", { status: 404 });
  });
}

function renderPage(): ReturnType<typeof render> {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const rootRoute = createRootRoute({ component: Outlet });
  const testRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/paths/$source/$target",
    component: PathDetail,
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
    history: createMemoryHistory({ initialEntries: ["/paths/a/b?range=24h"] }),
  });
  return render(
    <QueryClientProvider client={qc}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
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
});
