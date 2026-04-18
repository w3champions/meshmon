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
    // Needs at least one matching snapshot so the Report renders the
    // full section tree (the empty-state short-circuit skips the
    // Measurement timeline when no protocol-matching snapshots exist).
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
            { id: 1, observed_at: snap.observed_at, protocol: "icmp", path_summary: null },
          ],
          recent_snapshots_truncated: false,
          metrics: null,
          step: "1m",
        },
      },
      { url: /\/api\/paths\/.*\/routes\/1$/, status: 200, body: snap },
    ]);

    renderReport(defaultSearch);
    await screen.findByText(/metrics unavailable/i);
  });

  it("formats timestamps in UTC regardless of local timezone", async () => {
    const beforeSnap = {
      id: 1,
      source_id: "br",
      target_id: "fr",
      protocol: "icmp",
      // Pick wall-clock minutes that differ between common US timezones
      // and UTC — so if date-fns' `format` (which uses local tz) were
      // being called, the test would surface the drift.
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
      observed_at: "2026-04-13T13:30:00Z",
      path_summary: null,
      hops: [
        {
          position: 1,
          avg_rtt_micros: 2000,
          loss_pct: 0,
          stddev_rtt_micros: 0,
          observed_ips: [{ ip: "10.0.0.10", freq: 1 }],
        },
      ],
    };

    installFetchMock([
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
          latest_by_protocol: { icmp: afterSnap, tcp: null, udp: null },
          recent_snapshots: [
            { id: 2, observed_at: afterSnap.observed_at, protocol: "icmp", path_summary: null },
            { id: 1, observed_at: beforeSnap.observed_at, protocol: "icmp", path_summary: null },
          ],
          recent_snapshots_truncated: false,
          metrics: null,
          step: "1m",
        },
      },
      { url: /\/api\/paths\/.*\/routes\/1$/, status: 200, body: beforeSnap },
      { url: /\/api\/paths\/.*\/routes\/2$/, status: 200, body: afterSnap },
    ]);

    renderReport(defaultSearch);

    // Wait for AFTER snapshot timestamp (13:30Z) — last to render because
    // it depends on /routes/2 completing. Must render in UTC, not local
    // (which would be 15:30 in CEST, 06:30 in PST, etc.).
    await screen.findAllByText(/2026-04-13 13:30 UTC/);
    // Window line's upper bound (14:00Z) in UTC, not local.
    expect(screen.getAllByText(/2026-04-13 14:00 UTC/).length).toBeGreaterThan(0);
    // Window line lower bound (10:00Z) + BEFORE snapshot header both UTC.
    expect(screen.getAllByText(/2026-04-13 10:00 UTC/).length).toBeGreaterThan(0);
  });

  it("shows a destructive summary message when a snapshot fetch fails", async () => {
    // When BEFORE or AFTER snapshot fetches error out, `summary` stays
    // null — but so does the loading state, meaning the naive
    // `summary ? <ul> : <p>Computing…</p>` branch leaves the UI stuck
    // on "Computing…" forever. The Summary section must mirror the
    // BEFORE/AFTER error handling and surface an explicit destructive
    // message instead.
    const afterSnap = {
      id: 2,
      source_id: "br",
      target_id: "fr",
      protocol: "icmp",
      observed_at: "2026-04-13T13:30:00Z",
      path_summary: null,
      hops: [
        {
          position: 1,
          avg_rtt_micros: 2000,
          loss_pct: 0,
          stddev_rtt_micros: 0,
          observed_ips: [{ ip: "10.0.0.10", freq: 1 }],
        },
      ],
    };

    installFetchMock([
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
          latest_by_protocol: { icmp: afterSnap, tcp: null, udp: null },
          recent_snapshots: [
            { id: 2, observed_at: afterSnap.observed_at, protocol: "icmp", path_summary: null },
            {
              id: 1,
              observed_at: "2026-04-13T10:00:00Z",
              protocol: "icmp",
              path_summary: null,
            },
          ],
          recent_snapshots_truncated: false,
          metrics: null,
          step: "1m",
        },
      },
      // BEFORE snapshot fetch fails.
      { url: /\/api\/paths\/.*\/routes\/1$/, status: 500, body: { error: "boom" } },
      { url: /\/api\/paths\/.*\/routes\/2$/, status: 200, body: afterSnap },
    ]);

    renderReport(defaultSearch);

    // Wait for sections to mount.
    await screen.findByText("1.1.1.1");
    // Summary must surface the snapshot error, not a perpetual "Computing…".
    await screen.findByText(/summary unavailable.*snapshot fetch failed/i);
    expect(screen.queryByText(/^computing…$/i)).not.toBeInTheDocument();
  });

  it("shows an explicit error when a route-snapshot fetch fails", async () => {
    const afterSnap = {
      id: 2,
      source_id: "br",
      target_id: "fr",
      protocol: "icmp",
      observed_at: "2026-04-13T13:30:00Z",
      path_summary: null,
      hops: [
        {
          position: 1,
          avg_rtt_micros: 2000,
          loss_pct: 0,
          stddev_rtt_micros: 0,
          observed_ips: [{ ip: "10.0.0.10", freq: 1 }],
        },
      ],
    };

    installFetchMock([
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
          latest_by_protocol: { icmp: afterSnap, tcp: null, udp: null },
          recent_snapshots: [
            { id: 2, observed_at: afterSnap.observed_at, protocol: "icmp", path_summary: null },
            {
              id: 1,
              observed_at: "2026-04-13T10:00:00Z",
              protocol: "icmp",
              path_summary: null,
            },
          ],
          recent_snapshots_truncated: false,
          metrics: null,
          step: "1m",
        },
      },
      // BEFORE snapshot fetch fails — the report must surface this rather
      // than silently fall through to the "no snapshot available" copy.
      { url: /\/api\/paths\/.*\/routes\/1$/, status: 500, body: { error: "boom" } },
      { url: /\/api\/paths\/.*\/routes\/2$/, status: 200, body: afterSnap },
    ]);

    renderReport(defaultSearch);

    // Confirm the overview rendered so the sections are mounted.
    await screen.findByText("1.1.1.1");
    // The BEFORE section must show a destructive error, not the
    // "No BEFORE snapshot available." muted fallback.
    await screen.findByText(/failed to load before snapshot/i);
    expect(screen.queryByText(/no before snapshot available/i)).not.toBeInTheDocument();
  });

  it("passes diff to BEFORE table so removed hops render with diff highlight", async () => {
    // Previously only the AFTER RouteTable received `diff={routeDiff}`, so
    // hops removed between BEFORE and AFTER rendered in the BEFORE table
    // with no visual signal. Both tables must receive the diff — position
    // semantics mean `removed` only matches in BEFORE and `added` only in
    // AFTER, so there's no spurious cross-contamination.
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
        {
          position: 4,
          avg_rtt_micros: 4000,
          loss_pct: 0,
          stddev_rtt_micros: 0,
          observed_ips: [{ ip: "10.0.0.40", freq: 1 }],
        },
      ],
    };
    // AFTER drops position 4 — so BEFORE position 4 must be rendered as
    // "removed" in the BEFORE table.
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
          avg_rtt_micros: 1000,
          loss_pct: 0,
          stddev_rtt_micros: 0,
          observed_ips: [{ ip: "10.0.0.10", freq: 1 }],
        },
      ],
    };

    installFetchMock([
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
          latest_by_protocol: { icmp: afterSnap, tcp: null, udp: null },
          recent_snapshots: [
            { id: 2, observed_at: afterSnap.observed_at, protocol: "icmp", path_summary: null },
            { id: 1, observed_at: beforeSnap.observed_at, protocol: "icmp", path_summary: null },
          ],
          recent_snapshots_truncated: false,
          metrics: null,
          step: "1m",
        },
      },
      { url: /\/api\/paths\/.*\/routes\/1$/, status: 200, body: beforeSnap },
      { url: /\/api\/paths\/.*\/routes\/2$/, status: 200, body: afterSnap },
    ]);

    renderReport(defaultSearch);

    // Wait for BEFORE table to render its removed-only hop (10.0.0.40 exists
    // only in BEFORE because position 4 was dropped in AFTER).
    const removedCell = await screen.findByText("10.0.0.40");
    // Walk up to the TR — the diff state lives on the row element.
    const row = removedCell.closest("tr");
    expect(row).not.toBeNull();
    expect(row).toHaveAttribute("data-diff-state", "removed");
  });

  it("shows empty state when primary_protocol has no matching snapshots in window", async () => {
    // After T32 the backend scopes `recent_snapshots` to the resolved
    // `primary_protocol`, so when `?protocol=tcp` is in the URL but no
    // TCP snapshots exist in the window, the server returns an empty
    // `recent_snapshots` list. Without an empty-state guard,
    // `afterId`/`beforeId` both stay `undefined`, the snapshot queries
    // sit idle, `summary` stays null, and the Summary section renders
    // "Computing…" forever. The empty-state copy already suggests
    // adjusting range or protocol, which matches the new trigger
    // semantics perfectly.
    const icmpOnly = {
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

    installFetchMock([
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
          // Backend reports TCP as primary because `?protocol=tcp` was in
          // the URL, but no TCP snapshots exist in the window so the
          // protocol-scoped `recent_snapshots` query returns empty.
          primary_protocol: "tcp",
          latest_by_protocol: { icmp: icmpOnly, tcp: null, udp: null },
          recent_snapshots: [],
          recent_snapshots_truncated: false,
          metrics: null,
          step: "1m",
        },
      },
    ]);

    renderReport(`${defaultSearch}&protocol=tcp`);

    // Empty-state copy is the expected outcome.
    await screen.findByText(/no data in window/i);
    // Summary section and its "Computing…" placeholder must NOT render.
    expect(screen.queryByText(/^computing…$/i)).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: /^summary$/i })).not.toBeInTheDocument();
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
