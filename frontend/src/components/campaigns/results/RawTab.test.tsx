import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster, toast } from "sonner";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { Campaign, CampaignMeasurement, CampaignState } from "@/api/hooks/campaigns";
import { IpHostnameProvider } from "@/components/ip-hostname";

// ---------------------------------------------------------------------------
// EventSource stub — IpHostnameProvider opens an SSE connection on mount;
// jsdom has no native EventSource so we replace it with a no-op.
// ---------------------------------------------------------------------------

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

// ---------------------------------------------------------------------------
// Module mocks
// ---------------------------------------------------------------------------

const navigate = vi.fn();
let currentSearch: Record<string, unknown> = { tab: "raw" };

vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return {
    ...actual,
    useNavigate: () => navigate,
    useSearch: () => currentSearch,
  };
});

vi.mock("@/api/hooks/agents", async () => {
  const actual = await vi.importActual<typeof import("@/api/hooks/agents")>("@/api/hooks/agents");
  return { ...actual, useAgents: vi.fn() };
});

vi.mock("@/api/hooks/campaigns", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/campaigns")>("@/api/hooks/campaigns");
  return {
    ...actual,
    useCampaignMeasurements: vi.fn(),
    useForcePair: vi.fn(),
  };
});

// Stub RouteTopology — cytoscape does not run under jsdom.
vi.mock("@/components/RouteTopology", () => ({
  RouteTopology: () => <div data-testid="route-topology" />,
}));

import { useAgents } from "@/api/hooks/agents";
import { useCampaignMeasurements, useForcePair } from "@/api/hooks/campaigns";
import { RawTab } from "@/components/campaigns/results/RawTab";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

function makeCampaign(overrides: Partial<Campaign> & { state: CampaignState }): Campaign {
  return {
    id: overrides.id ?? CAMPAIGN_ID,
    title: overrides.title ?? "Campaign alpha",
    notes: overrides.notes ?? "",
    state: overrides.state,
    protocol: overrides.protocol ?? "icmp",
    evaluation_mode: overrides.evaluation_mode ?? "optimization",
    force_measurement: overrides.force_measurement ?? false,
    loss_threshold_ratio: overrides.loss_threshold_ratio ?? 0.02,
    stddev_weight: overrides.stddev_weight ?? 1,
    probe_count: overrides.probe_count ?? 10,
    probe_count_detail: overrides.probe_count_detail ?? 250,
    probe_stagger_ms: overrides.probe_stagger_ms ?? 100,
    timeout_ms: overrides.timeout_ms ?? 2000,
    created_at: overrides.created_at ?? "2026-04-01T12:00:00Z",
    created_by: overrides.created_by ?? "alice",
    started_at: overrides.started_at ?? null,
    stopped_at: overrides.stopped_at ?? null,
    completed_at: overrides.completed_at ?? null,
    evaluated_at: overrides.evaluated_at ?? null,
    pair_counts: overrides.pair_counts ?? [],
    max_hops: overrides.max_hops ?? 2,
    vm_lookback_minutes: overrides.vm_lookback_minutes ?? 15,
  };
}

function makeMeasurement(
  overrides: Partial<CampaignMeasurement> & { pair_id: number },
): CampaignMeasurement {
  return {
    pair_id: overrides.pair_id,
    source_agent_id: overrides.source_agent_id ?? "agent-a",
    destination_ip: overrides.destination_ip ?? `10.0.0.${overrides.pair_id}`,
    resolution_state: overrides.resolution_state ?? "succeeded",
    pair_kind: overrides.pair_kind ?? "campaign",
    protocol: overrides.protocol ?? "icmp",
    measurement_id: overrides.measurement_id ?? overrides.pair_id,
    measured_at: overrides.measured_at ?? "2026-04-21T09:55:00Z",
    latency_avg_ms: overrides.latency_avg_ms ?? 42,
    // Ratio 0.005 → LossChip renders "0.50%" (sits at the healthy boundary).
    loss_ratio: overrides.loss_ratio ?? 0.005,
    mtr_id: overrides.mtr_id ?? null,
    mtr_hops: overrides.mtr_hops ?? null,
  };
}

function makeAgent(id: string, display_name: string, ip: string): AgentSummary {
  return {
    id,
    display_name,
    ip,
    last_seen_at: "2026-04-21T10:00:00Z",
    registered_at: "2026-04-01T10:00:00Z",
  };
}

const forcePairStub = {
  mutate: vi.fn(),
  mutateAsync: vi.fn(),
  isPending: false,
  reset: vi.fn(),
};

interface MeasurementsQuerySetup {
  entries: CampaignMeasurement[];
  hasNextPage?: boolean;
  isFetchingNextPage?: boolean;
  fetchNextPage?: ReturnType<typeof vi.fn>;
  isLoading?: boolean;
  isError?: boolean;
}

function setupMocks(opts: MeasurementsQuerySetup) {
  vi.mocked(useCampaignMeasurements).mockReturnValue({
    data: {
      pages: [{ entries: opts.entries, next_cursor: opts.hasNextPage ? "next" : null }],
      pageParams: [null],
    },
    isLoading: opts.isLoading ?? false,
    isError: opts.isError ?? false,
    error: opts.isError ? new Error("boom") : null,
    hasNextPage: opts.hasNextPage ?? false,
    isFetchingNextPage: opts.isFetchingNextPage ?? false,
    fetchNextPage: opts.fetchNextPage ?? vi.fn(),
  } as unknown as ReturnType<typeof useCampaignMeasurements>);

  vi.mocked(useAgents).mockReturnValue({
    data: [makeAgent("agent-a", "alpha", "10.0.0.101"), makeAgent("agent-b", "beta", "10.0.0.102")],
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useAgents>);

  vi.mocked(useForcePair).mockReturnValue(
    forcePairStub as unknown as ReturnType<typeof useForcePair>,
  );
}

function renderTab(campaign: Campaign) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <IpHostnameProvider>
        <RawTab campaign={campaign} />
        <Toaster />
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
  navigate.mockReset();
  forcePairStub.mutate.mockReset();
  currentSearch = { tab: "raw" };
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  cleanup();
  toast.dismiss();
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("RawTab — render + states", () => {
  test("renders the filter bar and an empty placeholder when no rows match", () => {
    setupMocks({ entries: [] });
    renderTab(makeCampaign({ state: "running" }));

    expect(screen.getByTestId("raw-tab")).toBeInTheDocument();
    expect(screen.getByText(/resolution state/i)).toBeInTheDocument();
    expect(screen.getByText(/no measurements match/i)).toBeInTheDocument();
  });

  test("renders only a windowed slice of a large row set", () => {
    // 1000 rows — virtualizer should keep well under 200 DOM nodes.
    const entries = Array.from({ length: 1000 }, (_, i) => makeMeasurement({ pair_id: i + 1 }));
    setupMocks({ entries });
    renderTab(makeCampaign({ state: "running" }));

    const rows = screen.getAllByTestId(/raw-row-\d+/);
    // The exact count varies with virtualizer overscan, but it MUST be
    // dramatically less than 1000 to count as virtualized.
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThan(200);
  });

  test("renders error card on query failure", () => {
    setupMocks({ entries: [], isError: true });
    renderTab(makeCampaign({ state: "running" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/failed to load measurements/i);
  });
});

describe("RawTab — filter bar URL wiring", () => {
  test("clicking a state chip writes raw_state to the URL while preserving siblings", async () => {
    currentSearch = { tab: "raw", raw_protocol: "icmp" };
    setupMocks({ entries: [] });
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "running" }));

    await user.click(screen.getByTestId("raw-filter-resolution-state-pending"));

    expect(navigate).toHaveBeenCalledTimes(1);
    const call = navigate.mock.calls[0][0] as {
      search: Record<string, unknown>;
      replace: boolean;
    };
    expect(call.replace).toBe(true);
    // raw_protocol preserved from the initial search bag.
    expect(call.search.raw_protocol).toBe("icmp");
    expect(call.search.raw_state).toBe("pending");
    expect(call.search.tab).toBe("raw");
  });

  test("clicking the currently-selected chip clears it (sets param to undefined)", async () => {
    currentSearch = { tab: "raw", raw_kind: "detail_mtr" };
    setupMocks({ entries: [] });
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "running" }));

    await user.click(screen.getByTestId("raw-filter-kind-detail_mtr"));

    expect(navigate).toHaveBeenCalledTimes(1);
    const call = navigate.mock.calls[0][0] as { search: Record<string, unknown> };
    expect(call.search.raw_kind).toBeUndefined();
  });

  test("clicking 'All' chip resets the filter", async () => {
    currentSearch = { tab: "raw", raw_state: "pending" };
    setupMocks({ entries: [] });
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "running" }));

    await user.click(screen.getByTestId("raw-filter-resolution-state-all"));

    expect(navigate).toHaveBeenCalledTimes(1);
    const call = navigate.mock.calls[0][0] as { search: Record<string, unknown> };
    expect(call.search.raw_state).toBeUndefined();
  });
});

describe("RawTab — pagination", () => {
  test("virtualized scroll triggers fetchNextPage when rows approach the end", async () => {
    const fetchNextPage = vi.fn();
    // 20 rows + hasNextPage — virtualizer is eager, the effect should fire
    // on mount because the last-rendered row index is close to rows.length.
    const entries = Array.from({ length: 20 }, (_, i) => makeMeasurement({ pair_id: i + 1 }));
    setupMocks({ entries, hasNextPage: true, fetchNextPage });
    renderTab(makeCampaign({ state: "running" }));

    await waitFor(() => {
      expect(fetchNextPage).toHaveBeenCalled();
    });
  });

  test("renders the 'end of feed' footer when hasNextPage is false", () => {
    setupMocks({
      entries: [makeMeasurement({ pair_id: 1 })],
      hasNextPage: false,
    });
    renderTab(makeCampaign({ state: "running" }));
    expect(screen.getByText(/end of feed/i)).toBeInTheDocument();
  });

  test("surfaces the pending-tail caveat when a pending row is visible without a cursor", () => {
    // Backend emits `next_cursor=null` when a page saturates inside the
    // pending-row tail (the cursor is `measured_at`-keyed with NULLS LAST).
    // The generic "End of feed" would hide remaining in-flight work; the
    // footer must instead steer the operator at the `Resolution state`
    // filter that actually enumerates pending/dispatched rows.
    setupMocks({
      entries: [
        makeMeasurement({ pair_id: 1, resolution_state: "succeeded" }),
        makeMeasurement({ pair_id: 2, resolution_state: "pending" }),
      ],
      hasNextPage: false,
    });
    renderTab(makeCampaign({ state: "running" }));
    expect(screen.getByText(/end of settled measurements/i)).toBeInTheDocument();
    expect(screen.queryByText(/^end of feed —/i)).toBeNull();
  });

  test("keeps the generic footer when the operator already narrowed by resolution_state", () => {
    // If the filter bar is already scoping to `pending`, the pending-tail
    // caveat would be redundant — the user is already seeing that bucket.
    currentSearch = { tab: "raw", raw_state: "pending" };
    setupMocks({
      entries: [makeMeasurement({ pair_id: 1, resolution_state: "pending" })],
      hasNextPage: false,
    });
    renderTab(makeCampaign({ state: "running" }));
    expect(screen.getByText(/^end of feed —/i)).toBeInTheDocument();
  });
});

describe("RawTab — per-row navigation", () => {
  test("clicking the view-history affordance navigates to /history/pair with source + destination", async () => {
    setupMocks({
      entries: [
        makeMeasurement({
          pair_id: 1,
          source_agent_id: "agent-a",
          destination_ip: "10.0.0.42",
        }),
      ],
    });
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "running" }));

    await user.click(screen.getByTestId("raw-row-0-history"));

    expect(navigate).toHaveBeenCalledTimes(1);
    const call = navigate.mock.calls[0][0] as {
      to: string;
      search: { source: string; destination: string };
    };
    expect(call.to).toBe("/history/pair");
    expect(call.search).toEqual({ source: "agent-a", destination: "10.0.0.42" });
  });
});
