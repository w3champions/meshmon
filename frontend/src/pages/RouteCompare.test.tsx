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
import RouteCompare from "@/pages/RouteCompare";

afterEach(() => vi.restoreAllMocks());

function snap(id: number, ip: string, rttUs: number) {
  return {
    id,
    source_id: "a",
    target_id: "b",
    protocol: "icmp",
    observed_at: "2026-04-13T10:00:00Z",
    hops: [
      {
        position: 1,
        observed_ips: [{ ip, freq: 1 }],
        avg_rtt_micros: rttUs,
        stddev_rtt_micros: 100,
        loss_pct: 0,
      },
    ],
  };
}

describe("RouteCompare", () => {
  test("fetches both snapshots and renders the diff summary", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = typeof input === "string" ? input : (input as Request).url;
      if (url.endsWith("/101")) return new Response(JSON.stringify(snap(101, "10.0.0.1", 1_000)));
      if (url.endsWith("/102")) return new Response(JSON.stringify(snap(102, "10.0.0.2", 1_000)));
      if (url.includes("/api/web-config")) {
        return new Response(
          JSON.stringify({ version: "0", username: "u", grafana_dashboards: {} }),
          { status: 200 },
        );
      }
      return new Response("nf", { status: 404 });
    });

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const rootRoute = createRootRoute({ component: Outlet });
    const testRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: "/paths/$source/$target/routes/compare",
      component: RouteCompare,
    });
    const router = createRouter({
      routeTree: rootRoute.addChildren([testRoute]),
      history: createMemoryHistory({
        initialEntries: ["/paths/a/b/routes/compare?a=101&b=102"],
      }),
    });
    render(
      <QueryClientProvider client={qc}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
    expect(await screen.findByText(/1 changed/i)).toBeInTheDocument();
  });

  test("shows an error message when either snapshot can't be loaded", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(async () => new Response("", { status: 404 }));
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const rootRoute = createRootRoute({ component: Outlet });
    const testRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: "/paths/$source/$target/routes/compare",
      component: RouteCompare,
    });
    const router = createRouter({
      routeTree: rootRoute.addChildren([testRoute]),
      history: createMemoryHistory({
        initialEntries: ["/paths/a/b/routes/compare?a=1&b=2"],
      }),
    });
    render(
      <QueryClientProvider client={qc}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
    expect(
      await screen.findByText(/one of the snapshots could not be loaded/i),
    ).toBeInTheDocument();
  });
});
