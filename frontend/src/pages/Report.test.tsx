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
import { afterEach, describe, expect, it, vi } from "vitest";
import Report from "./Report";

interface MockResponse {
  url: RegExp;
  status: number;
  body: unknown;
}

function installFetchMock(responses: MockResponse[]) {
  return vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = typeof input === "string" ? input : (input as Request).url;
    const hit = responses.find((r) => r.url.test(url));
    if (!hit) throw new Error(`unmocked fetch: ${url}`);
    return new Response(JSON.stringify(hit.body), {
      status: hit.status,
      headers: { "content-type": "application/json" },
    });
  });
}

function renderReport(search: string) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const rootRoute = createRootRoute({ component: Outlet });
  const reportRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/reports/path",
    component: Report,
    validateSearch: (s: Record<string, unknown>) => ({
      source_id: String(s.source_id ?? ""),
      target_id: String(s.target_id ?? ""),
      from: String(s.from ?? ""),
      to: String(s.to ?? ""),
      protocol:
        s.protocol === "icmp" || s.protocol === "tcp" || s.protocol === "udp"
          ? s.protocol
          : undefined,
    }),
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([reportRoute]),
    history: createMemoryHistory({
      initialEntries: [`/reports/path${search}`],
    }),
  });
  return render(
    <QueryClientProvider client={qc}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

afterEach(() => vi.restoreAllMocks());

const defaultSearch =
  "?source_id=br&target_id=fr&from=2026-04-13T10:00:00.000Z&to=2026-04-13T14:00:00.000Z";

describe("Report page", () => {
  it("shows empty state when primary_protocol is null", async () => {
    installFetchMock([
      {
        url: /\/api\/web-config$/,
        status: 200,
        body: {
          username: "u",
          version: "v",
          grafana_dashboards: {},
        },
      },
      {
        url: /\/api\/paths\/.*\/overview/,
        status: 200,
        body: {
          source: {
            id: "br",
            display_name: "BR",
            ip: "10.0.0.1",
            registered_at: "2026-01-01T00:00:00Z",
            last_seen_at: new Date().toISOString(),
          },
          target: {
            id: "fr",
            display_name: "FR",
            ip: "10.0.0.2",
            registered_at: "2026-01-01T00:00:00Z",
            last_seen_at: new Date().toISOString(),
          },
          window: {
            from: "2026-04-13T10:00:00Z",
            to: "2026-04-13T14:00:00Z",
          },
          primary_protocol: null,
          latest_by_protocol: { icmp: null, tcp: null, udp: null },
          recent_snapshots: [],
          recent_snapshots_truncated: false,
          metrics: null,
          step: "1m",
        },
      },
    ]);

    renderReport(defaultSearch);

    await screen.findByText(/no data in window/i);
    // Header still renders so operators can confirm they opened the right URL.
    expect(screen.getByText("10.0.0.1")).toBeInTheDocument();
    expect(screen.getByText("10.0.0.2")).toBeInTheDocument();
  });

  it("renders header, summary, and both route tables when data is present", async () => {
    const beforeSnap = {
      id: 1,
      source_id: "br",
      target_id: "fr",
      protocol: "icmp",
      observed_at: "2026-04-13T10:00:00Z",
      path_summary: null,
      hops: [
        {
          position: 1,
          avg_rtt_micros: 1000,
          loss_pct: 0,
          stddev_rtt_micros: 0,
          observed_ips: [{ ip: "10.0.0.10", freq: 1 }],
        },
      ],
    };
    const afterSnap = {
      id: 2,
      source_id: "br",
      target_id: "fr",
      protocol: "icmp",
      observed_at: "2026-04-13T13:00:00Z",
      path_summary: null,
      hops: [
        {
          position: 1,
          avg_rtt_micros: 2000,
          loss_pct: 0.05,
          stddev_rtt_micros: 0,
          observed_ips: [{ ip: "10.0.9.99", freq: 1 }],
        },
      ],
    };

    installFetchMock([
      {
        url: /\/api\/web-config$/,
        status: 200,
        body: {
          username: "u",
          version: "v",
          grafana_dashboards: {},
        },
      },
      {
        url: /\/api\/paths\/.*\/overview/,
        status: 200,
        body: {
          source: {
            id: "br",
            display_name: "BR",
            ip: "170.80.110.90",
            registered_at: "2026-01-01T00:00:00Z",
            last_seen_at: new Date().toISOString(),
          },
          target: {
            id: "fr",
            display_name: "FR",
            ip: "85.90.216.7",
            registered_at: "2026-01-01T00:00:00Z",
            last_seen_at: new Date().toISOString(),
          },
          window: {
            from: "2026-04-13T10:00:00Z",
            to: "2026-04-13T14:00:00Z",
          },
          primary_protocol: "icmp",
          latest_by_protocol: { icmp: afterSnap, tcp: null, udp: null },
          recent_snapshots: [
            {
              id: 2,
              observed_at: afterSnap.observed_at,
              protocol: "icmp",
              path_summary: null,
            },
            {
              id: 1,
              observed_at: beforeSnap.observed_at,
              protocol: "icmp",
              path_summary: null,
            },
          ],
          recent_snapshots_truncated: false,
          metrics: {
            rtt_current: 2,
            loss_current: 0.05,
            rtt_series: [
              [1, 1],
              [2, 2],
            ],
            loss_series: [
              [1, 0.001],
              [2, 0.05],
            ],
          },
          step: "1m",
        },
      },
      { url: /\/api\/paths\/.*\/routes\/1$/, status: 200, body: beforeSnap },
      { url: /\/api\/paths\/.*\/routes\/2$/, status: 200, body: afterSnap },
    ]);

    renderReport(`${defaultSearch}&protocol=icmp`);

    await screen.findByText("170.80.110.90");
    expect(screen.getByText("85.90.216.7")).toBeInTheDocument();
    // Protocol label lives in the header grid as its own cell, uppercased
    // via CSS (`text-transform: uppercase`); raw text is the lowercase
    // identifier from the overview body.
    const protocolCell = screen.getByText("icmp");
    expect(protocolCell).toHaveClass("uppercase");
    await screen.findByText("10.0.9.99");
    expect(screen.getByText("10.0.0.10")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /export pdf/i })).toBeInTheDocument();
  });

  it("shows 'metrics unavailable' when metrics is null", async () => {
    installFetchMock([
      {
        url: /\/api\/web-config$/,
        status: 200,
        body: { username: "u", version: "v", grafana_dashboards: {} },
      },
      {
        url: /\/api\/paths\/.*\/overview/,
        status: 200,
        body: {
          source: {
            id: "br",
            display_name: "BR",
            ip: "1.1.1.1",
            registered_at: "2026-01-01T00:00:00Z",
            last_seen_at: new Date().toISOString(),
          },
          target: {
            id: "fr",
            display_name: "FR",
            ip: "2.2.2.2",
            registered_at: "2026-01-01T00:00:00Z",
            last_seen_at: new Date().toISOString(),
          },
          window: {
            from: "2026-04-13T10:00:00Z",
            to: "2026-04-13T14:00:00Z",
          },
          primary_protocol: "icmp",
          latest_by_protocol: { icmp: null, tcp: null, udp: null },
          recent_snapshots: [],
          recent_snapshots_truncated: false,
          metrics: null,
          step: "1m",
        },
      },
    ]);

    renderReport(defaultSearch);
    await screen.findByText(/metrics unavailable/i);
  });

  it("shows truncation banner when recent_snapshots_truncated is true", async () => {
    const snap = {
      id: 1,
      source_id: "br",
      target_id: "fr",
      protocol: "icmp",
      observed_at: "2026-04-13T10:00:00Z",
      path_summary: null,
      hops: [
        {
          position: 1,
          avg_rtt_micros: 1000,
          loss_pct: 0,
          stddev_rtt_micros: 0,
          observed_ips: [{ ip: "10.0.0.1", freq: 1 }],
        },
      ],
    };
    installFetchMock([
      {
        url: /\/api\/web-config$/,
        status: 200,
        body: { username: "u", version: "v", grafana_dashboards: {} },
      },
      {
        url: /\/api\/paths\/.*\/overview/,
        status: 200,
        body: {
          source: {
            id: "br",
            display_name: "BR",
            ip: "1.1.1.1",
            registered_at: "2026-01-01T00:00:00Z",
            last_seen_at: new Date().toISOString(),
          },
          target: {
            id: "fr",
            display_name: "FR",
            ip: "2.2.2.2",
            registered_at: "2026-01-01T00:00:00Z",
            last_seen_at: new Date().toISOString(),
          },
          window: {
            from: "2026-04-13T10:00:00Z",
            to: "2026-04-13T14:00:00Z",
          },
          primary_protocol: "icmp",
          latest_by_protocol: { icmp: snap, tcp: null, udp: null },
          recent_snapshots: [
            {
              id: 1,
              observed_at: snap.observed_at,
              protocol: "icmp",
              path_summary: null,
            },
          ],
          recent_snapshots_truncated: true,
          metrics: null,
          step: "1m",
        },
      },
      { url: /\/api\/paths\/.*\/routes\/1$/, status: 200, body: snap },
    ]);

    renderReport(defaultSearch);
    await screen.findByText(/showing latest 100/i);
  });
});
