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
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { z } from "zod";
import "@/test/cytoscape-mock";
import type { HistoryMeasurement } from "@/api/hooks/history";
import HistoryPair, { HISTORY_MEASUREMENTS_CAP } from "@/pages/HistoryPair";

afterEach(() => vi.restoreAllMocks());

const SOURCES = [
  { source_agent_id: "agent-a", display_name: "Agent A" },
  { source_agent_id: "agent-b", display_name: "Agent B" },
];

const DEST_WITH_META = {
  destination_ip: "10.0.0.1",
  display_name: "router-1",
  city: "Frankfurt",
  country_code: "DE",
  asn: 64512,
  is_mesh_member: false,
};

const DEST_RAW_ONLY = {
  destination_ip: "10.0.0.2",
  display_name: "10.0.0.2",
  city: null,
  country_code: null,
  asn: null,
  is_mesh_member: false,
};

function measurement(over: Partial<HistoryMeasurement> = {}): HistoryMeasurement {
  return {
    id: 1,
    source_agent_id: "agent-a",
    destination_ip: "10.0.0.1",
    protocol: "icmp",
    kind: "campaign",
    measured_at: "2026-04-20T00:00:00.000Z",
    probe_count: 10,
    loss_pct: 0.1,
    latency_avg_ms: 12,
    latency_min_ms: 10,
    latency_max_ms: 15,
    latency_p95_ms: 14,
    latency_stddev_ms: 1,
    mtr_captured_at: null,
    mtr_hops: null,
    ...over,
  };
}

interface MockFixture {
  sources?: unknown;
  destinations?: unknown;
  measurements?: HistoryMeasurement[];
}

function installFetchMock(fixture: MockFixture): void {
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = typeof input === "string" ? input : (input as Request).url;
    if (url.includes("/api/history/sources")) {
      return new Response(JSON.stringify(fixture.sources ?? SOURCES), { status: 200 });
    }
    if (url.includes("/api/history/destinations")) {
      return new Response(JSON.stringify(fixture.destinations ?? [DEST_WITH_META, DEST_RAW_ONLY]), {
        status: 200,
      });
    }
    if (url.includes("/api/history/measurements")) {
      return new Response(JSON.stringify(fixture.measurements ?? []), { status: 200 });
    }
    return new Response("nf", { status: 404 });
  });
}

/**
 * Mirror of `historyPairSearchSchema` but looser — the tests care about
 * exercising the page, not the schema (which is tested via the production
 * router integration by construction).
 */
const searchSchema = z
  .object({
    source: z.string().optional(),
    destination: z.string().optional(),
    protocol: z.array(z.enum(["icmp", "tcp", "udp"])).optional(),
    range: z.enum(["24h", "7d", "30d", "90d", "custom"]).default("30d"),
    from: z.string().datetime().optional(),
    to: z.string().datetime().optional(),
  })
  .refine((s) => s.range !== "custom" || (s.from && s.to), {
    message: "custom range requires from and to",
  });

function renderHistoryPair(initialUrl: string) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const rootRoute = createRootRoute({ component: Outlet });
  const pageRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/history/pair",
    component: HistoryPair,
    validateSearch: (search: Record<string, unknown>) => searchSchema.parse(search),
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([pageRoute]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
  return render(
    <QueryClientProvider client={qc}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

describe("HistoryPair", () => {
  test("renders the empty state when no source is picked", async () => {
    installFetchMock({});
    renderHistoryPair("/history/pair");
    expect(await screen.findByText(/pick a source to begin/i)).toBeInTheDocument();
  });

  test("preseeds pickers from ?source&destination and renders the chart + MTR section", async () => {
    installFetchMock({ measurements: [measurement()] });
    renderHistoryPair("/history/pair?source=agent-a&destination=10.0.0.1");

    // Picker triggers show the resolved labels once sources resolve.
    const sourceTrigger = await screen.findByRole("combobox", { name: /source picker/i });
    await waitFor(() => expect(sourceTrigger).toHaveTextContent("Agent A"));

    const destTrigger = await screen.findByRole("combobox", { name: /destination picker/i });
    await waitFor(() => expect(destTrigger).toHaveTextContent(/router-1|10\.0\.0\.1/));

    // Chart section mounts once measurements arrive.
    expect(await screen.findByRole("img", { name: /latency over time/i })).toBeInTheDocument();
    expect(
      await screen.findByRole("heading", { level: 2, name: /mtr traces/i }),
    ).toBeInTheDocument();
  });

  test("shows the 5000-row notice when the response length hits the cap", async () => {
    const big: HistoryMeasurement[] = Array.from({ length: HISTORY_MEASUREMENTS_CAP }, (_, i) =>
      measurement({
        id: i + 1,
        measured_at: new Date(Date.UTC(2026, 3, 20, 0, 0, i)).toISOString(),
      }),
    );
    installFetchMock({ measurements: big });
    renderHistoryPair("/history/pair?source=agent-a&destination=10.0.0.1");

    const notice = await screen.findByTestId("history-pair-cap-notice");
    expect(notice).toHaveTextContent(/most recent 5,000/i);
  });

  test("renders a raw-IP fallback when the catalogue metadata is missing", async () => {
    installFetchMock({});
    renderHistoryPair("/history/pair?source=agent-a");

    // Open the destination picker so the list renders.
    const destTrigger = await screen.findByRole("combobox", { name: /destination picker/i });
    destTrigger.click();

    // The option without catalogue metadata surfaces the raw IP plus the
    // "no metadata" tag instead of a formatted display name.
    expect(await screen.findByText(/— no metadata/i)).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// Keyboard-accessible popover pickers (WAI-ARIA filterable-listbox pattern).
// Virtual focus stays on the filter `<Input>`; `ArrowUp/Down` move the
// `aria-activedescendant` across options, `Enter` commits the focused one.
// ---------------------------------------------------------------------------
describe("HistoryPair keyboard-accessible pickers", () => {
  test("ArrowDown on an open source picker moves aria-activedescendant across options", async () => {
    installFetchMock({});
    renderHistoryPair("/history/pair");

    const user = userEvent.setup();
    const trigger = await screen.findByRole("combobox", { name: /source picker/i });
    await user.click(trigger);

    const filter = await screen.findByRole("searchbox", { name: /filter sources/i });
    // Initial state: no option focused so active-descendant is empty.
    expect(filter.getAttribute("aria-activedescendant")).toBeFalsy();

    // Wait for the options to render before keying; the async source list
    // only resolves after the mocked fetch completes.
    await screen.findByRole("option", { name: /Agent A/i });

    await user.keyboard("{ArrowDown}");
    await waitFor(() =>
      expect(filter.getAttribute("aria-activedescendant")).toBe("source-opt-agent-a"),
    );

    await user.keyboard("{ArrowDown}");
    await waitFor(() =>
      expect(filter.getAttribute("aria-activedescendant")).toBe("source-opt-agent-b"),
    );
  });

  test("Enter on the source picker selects the focused option and closes the popover", async () => {
    installFetchMock({});
    renderHistoryPair("/history/pair");

    const user = userEvent.setup();
    const trigger = await screen.findByRole("combobox", { name: /source picker/i });
    await user.click(trigger);

    await screen.findByRole("option", { name: /Agent A/i });

    // Arrow down onto the first option, then commit with Enter. The URL
    // picks up `?source=agent-a` and the trigger relabels to "Agent A".
    await user.keyboard("{ArrowDown}");
    await user.keyboard("{Enter}");

    await waitFor(() => expect(trigger).toHaveTextContent(/Agent A/i));
    // Popover closes on select — filter input no longer in the DOM.
    await waitFor(() =>
      expect(screen.queryByRole("searchbox", { name: /filter sources/i })).toBeNull(),
    );
  });

  test("typing into the source filter resets the focused index", async () => {
    installFetchMock({});
    renderHistoryPair("/history/pair");

    const user = userEvent.setup();
    const trigger = await screen.findByRole("combobox", { name: /source picker/i });
    await user.click(trigger);

    await screen.findByRole("option", { name: /Agent A/i });
    const filter = await screen.findByRole("searchbox", { name: /filter sources/i });

    // Move focus onto the first option.
    await user.keyboard("{ArrowDown}");
    await waitFor(() =>
      expect(filter.getAttribute("aria-activedescendant")).toBe("source-opt-agent-a"),
    );

    // A new keystroke refreshes the query which resets focus to -1.
    await user.type(filter, "a");
    await waitFor(() => expect(filter.getAttribute("aria-activedescendant")).toBeFalsy());
  });

  test("ArrowDown on an open destination picker moves aria-activedescendant across options", async () => {
    installFetchMock({});
    renderHistoryPair("/history/pair?source=agent-a");

    const user = userEvent.setup();
    const destTrigger = await screen.findByRole("combobox", { name: /destination picker/i });
    await user.click(destTrigger);

    const destFilter = await screen.findByRole("searchbox", { name: /filter destinations/i });
    await screen.findByRole("option", { name: /router-1/i });

    await user.keyboard("{ArrowDown}");
    await waitFor(() =>
      expect(destFilter.getAttribute("aria-activedescendant")).toBe("dest-opt-10.0.0.1"),
    );
    await user.keyboard("{ArrowDown}");
    await waitFor(() =>
      expect(destFilter.getAttribute("aria-activedescendant")).toBe("dest-opt-10.0.0.2"),
    );
  });
});
