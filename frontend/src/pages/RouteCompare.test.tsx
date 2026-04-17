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
import RouteCompare from "@/pages/RouteCompare";

afterEach(() => vi.restoreAllMocks());

function detail(id: number, observed_at: string, hops: unknown[] = []) {
  return {
    id,
    source_id: "fra-01",
    target_id: "nyc-02",
    protocol: "tcp",
    observed_at,
    hops,
  };
}

function hop(position: number, ip: string, rttUs: number) {
  return {
    position,
    observed_ips: [{ ip, freq: 1 }],
    avg_rtt_micros: rttUs,
    stddev_rtt_micros: 100,
    loss_pct: 0,
  };
}

function listRoutesResponse(ids: Array<{ id: number; observed_at: string }>) {
  return {
    items: ids.map((x) => ({
      id: x.id,
      source_id: "fra-01",
      target_id: "nyc-02",
      protocol: "tcp",
      observed_at: x.observed_at,
    })),
    limit: 500,
    offset: 0,
  };
}

function makeRouter(initialUrl: string) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const rootRoute = createRootRoute({ component: Outlet });
  const testRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/paths/$source/$target/routes/compare",
    component: RouteCompare,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([testRoute]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
  return { qc, router };
}

describe("RouteCompare (redesigned)", () => {
  test("renders stacked RouteTables with shared diff tinting and no cytoscape canvas", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = typeof input === "string" ? input : (input as Request).url;
      if (url.endsWith("/101")) {
        return new Response(
          JSON.stringify(detail(101, "2026-04-17T09:12:04Z", [hop(1, "10.0.0.1", 1_000)])),
        );
      }
      if (url.endsWith("/102")) {
        return new Response(
          JSON.stringify(detail(102, "2026-04-17T09:14:41Z", [hop(1, "10.0.0.9", 1_200)])),
        );
      }
      if (url.includes("/api/paths/fra-01/nyc-02/routes") && url.includes("from=")) {
        return new Response(
          JSON.stringify(
            listRoutesResponse([
              { id: 99, observed_at: "2026-04-17T09:10:00Z" },
              { id: 101, observed_at: "2026-04-17T09:12:04Z" },
              { id: 102, observed_at: "2026-04-17T09:14:41Z" },
              { id: 103, observed_at: "2026-04-17T09:17:00Z" },
            ]),
          ),
        );
      }
      if (url.includes("/api/web-config")) {
        return new Response(
          JSON.stringify({ version: "0", username: "u", grafana_dashboards: {} }),
        );
      }
      return new Response("nf", { status: 404 });
    });

    const { qc, router } = makeRouter("/paths/fra-01/nyc-02/routes/compare?a=101&b=102");
    render(
      <QueryClientProvider client={qc}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );

    expect(await screen.findByText(/1 changed/i)).toBeInTheDocument();
    expect(screen.getAllByText(/09:12(?::04)?/).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/09:14(?::41)?/).length).toBeGreaterThan(0);

    const tables = screen.getAllByRole("table");
    expect(tables.length).toBe(2);

    expect(document.querySelector("canvas[data-cy='cytoscape']")).toBeNull();
    expect(document.querySelectorAll(".cy-container").length).toBe(0);
  });

  test("shows the error message when either snapshot can't be loaded", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(async () => new Response("", { status: 404 }));
    const { qc, router } = makeRouter("/paths/fra-01/nyc-02/routes/compare?a=1&b=2");
    render(
      <QueryClientProvider client={qc}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
    expect(
      await screen.findByText(/one of the snapshots could not be loaded/i),
    ).toBeInTheDocument();
  });

  test("keyboard shortcuts are ignored when a modifier key is held", async () => {
    const { default: userEvent } = await import("@testing-library/user-event");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = typeof input === "string" ? input : (input as Request).url;
      if (url.endsWith("/101")) {
        return new Response(JSON.stringify(detail(101, "2026-04-17T09:12:04Z")));
      }
      if (url.endsWith("/102")) {
        return new Response(JSON.stringify(detail(102, "2026-04-17T09:14:41Z")));
      }
      if (url.includes("/api/paths/fra-01/nyc-02/routes") && url.includes("from=")) {
        return new Response(
          JSON.stringify(
            listRoutesResponse([
              { id: 100, observed_at: "2026-04-17T09:10:00Z" },
              { id: 101, observed_at: "2026-04-17T09:12:04Z" },
              { id: 102, observed_at: "2026-04-17T09:14:41Z" },
              { id: 103, observed_at: "2026-04-17T09:17:00Z" },
            ]),
          ),
        );
      }
      return new Response("nf", { status: 404 });
    });
    const { qc, router } = makeRouter("/paths/fra-01/nyc-02/routes/compare?a=101&b=102");
    const user = userEvent.setup();
    render(
      <QueryClientProvider client={qc}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
    await screen.findAllByRole("button", { name: /jump/i });
    const urlBefore = router.state.location.search;

    await user.keyboard("{Meta>}j{/Meta}");
    await user.keyboard("{Control>}k{/Control}");
    await user.keyboard("{Alt>}l{/Alt}");
    await user.keyboard("{Shift>};{/Shift}");

    expect(router.state.location.search).toEqual(urlBefore);
  });

  test("pressing G clicks the Jump trigger of the focused card (defaulting to A)", async () => {
    const { default: userEvent } = await import("@testing-library/user-event");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = typeof input === "string" ? input : (input as Request).url;
      if (url.endsWith("/101")) {
        return new Response(JSON.stringify(detail(101, "2026-04-17T09:12:04Z")));
      }
      if (url.endsWith("/102")) {
        return new Response(JSON.stringify(detail(102, "2026-04-17T09:14:41Z")));
      }
      if (url.includes("/api/paths/fra-01/nyc-02/routes") && url.includes("from=")) {
        return new Response(
          JSON.stringify(
            listRoutesResponse([
              { id: 101, observed_at: "2026-04-17T09:12:04Z" },
              { id: 102, observed_at: "2026-04-17T09:14:41Z" },
            ]),
          ),
        );
      }
      return new Response("nf", { status: 404 });
    });

    const { qc, router } = makeRouter("/paths/fra-01/nyc-02/routes/compare?a=101&b=102");
    const user = userEvent.setup();
    render(
      <QueryClientProvider client={qc}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
    const triggers = await screen.findAllByRole("button", { name: /jump/i });
    expect(triggers.length).toBeGreaterThan(0);

    await user.keyboard("g");
    expect(await screen.findByRole("button", { name: /^-5m$/ })).toBeInTheDocument();
  });
});
