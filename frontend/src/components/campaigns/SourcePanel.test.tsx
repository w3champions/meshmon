import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { type ReactElement, type ReactNode, useEffect } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import * as agentsHook from "@/api/hooks/agents";
import * as livenessHook from "@/api/hooks/liveness";
import { SourcePanel } from "@/components/campaigns/SourcePanel";
import type { FilterValue } from "@/components/filter/FilterRail";
import { useIpHostnameContext } from "@/components/ip-hostname/IpHostnameProvider";
import { DEFAULT_LIVENESS_THRESHOLDS } from "@/lib/health";
import { renderWithQuery } from "@/test/query-wrapper";

class MockEventSource {
  static instances: MockEventSource[] = [];
  listeners: Record<string, Array<(event: { data: string }) => void>> = {};
  constructor(public url: string) {
    MockEventSource.instances.push(this);
  }
  addEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name] ?? [];
    list.push(handler);
    this.listeners[name] = list;
  }
  removeEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name];
    if (!list) return;
    const idx = list.indexOf(handler);
    if (idx >= 0) list.splice(idx, 1);
  }
  close(): void {}
}

interface SeedEntry {
  ip: string;
  hostname?: string | null;
}

function Seeder({ seed, children }: { seed: SeedEntry[]; children: ReactNode }) {
  const { seedFromResponse } = useIpHostnameContext();
  // biome-ignore lint/correctness/useExhaustiveDependencies: mount-only seed
  useEffect(() => {
    if (seed.length > 0) seedFromResponse(seed);
  }, []);
  return <>{children}</>;
}

function renderPanel(ui: ReactElement, seed: SeedEntry[] = []) {
  return renderWithQuery(<Seeder seed={seed}>{ui}</Seeder>);
}

vi.mock("@/api/hooks/agents");
// `useAgentLivenessThresholds` calls `useSession` which calls
// `useQuery`. RTL's `rerender` does not re-wrap with the QueryClient
// provider, so without this mock the second render in a `rerender`-style
// test crashes with "No QueryClient set". Mocking with the library
// defaults keeps the offline/stale thresholds aligned with the existing
// fixtures (5 min offline, 20 s stale).
vi.mock("@/api/hooks/liveness");

const FUTURE = new Date(Date.now() + 60_000).toISOString();
const STALE = new Date(Date.now() - 10 * 60_000).toISOString();

const AGENT_BERLIN: AgentSummary = {
  id: "berlin-1",
  display_name: "Berlin Alpha",
  ip: "10.0.0.1",
  last_seen_at: FUTURE,
  registered_at: "2026-01-01T00:00:00Z",
  catalogue_coordinates: { latitude: 52.52, longitude: 13.4 },
};

const AGENT_AMSTERDAM: AgentSummary = {
  id: "ams-1",
  display_name: "Amsterdam Beta",
  ip: "10.0.0.2",
  last_seen_at: FUTURE,
  registered_at: "2026-01-01T00:00:00Z",
  catalogue_coordinates: { latitude: 52.37, longitude: 4.9 },
};

const AGENT_STALE: AgentSummary = {
  id: "stale-1",
  display_name: "Stale Agent",
  ip: "10.0.0.3",
  last_seen_at: STALE,
  registered_at: "2026-01-01T00:00:00Z",
  catalogue_coordinates: { latitude: 40.0, longitude: -3.7 },
};

const AGENT_NO_COORDS: AgentSummary = {
  id: "nogeo-1",
  display_name: "No Coords",
  ip: "10.0.0.4",
  last_seen_at: FUTURE,
  registered_at: "2026-01-01T00:00:00Z",
  catalogue_coordinates: null,
};

const EMPTY_FILTER: FilterValue = {
  countryCodes: [],
  asns: [],
  networks: [],
  cities: [],
  shapes: [],
};

function mockAgents(agents: AgentSummary[]) {
  vi.mocked(agentsHook.useAgents).mockReturnValue({
    data: agents,
    isLoading: false,
    isError: false,
  } as ReturnType<typeof agentsHook.useAgents>);
  vi.mocked(livenessHook.useAgentLivenessThresholds).mockReturnValue(DEFAULT_LIVENESS_THRESHOLDS);
}

beforeEach(() => {
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("SourcePanel", () => {
  test("renders rows for each agent returned by useAgents", () => {
    mockAgents([AGENT_BERLIN, AGENT_AMSTERDAM]);

    renderWithQuery(
      <SourcePanel
        selected={new Set()}
        onSelectedChange={vi.fn()}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    expect(screen.getByText("Berlin Alpha")).toBeInTheDocument();
    expect(screen.getByText("Amsterdam Beta")).toBeInTheDocument();
  });

  test("'Add all' snapshots matching ids and is unaffected by later filter changes", async () => {
    mockAgents([AGENT_BERLIN, AGENT_AMSTERDAM]);
    const onSelectedChange = vi.fn<(next: Set<string>) => void>();
    const user = userEvent.setup();

    // Filter to Berlin only via nameSearch so the filter predicate runs
    // against the agent list.
    const berlinFilter: FilterValue = { ...EMPTY_FILTER, nameSearch: "Berlin" };

    const { rerender } = renderWithQuery(
      <SourcePanel
        selected={new Set()}
        onSelectedChange={onSelectedChange}
        filter={berlinFilter}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: /add all/i }));
    expect(onSelectedChange).toHaveBeenCalledTimes(1);
    const snapshot = onSelectedChange.mock.calls[0]?.[0];
    expect(Array.from(snapshot ?? [])).toEqual(["berlin-1"]);

    // Re-render with an empty filter; `selected` is the parent-owned prop
    // and must NOT change just because the filter widened.
    onSelectedChange.mockClear();
    rerender(
      <SourcePanel
        selected={snapshot ?? new Set()}
        onSelectedChange={onSelectedChange}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );
    expect(onSelectedChange).not.toHaveBeenCalled();
  });

  test("renders offline badge for agents with last_seen_at older than 5 minutes", () => {
    mockAgents([AGENT_BERLIN, AGENT_STALE]);

    renderWithQuery(
      <SourcePanel
        selected={new Set()}
        onSelectedChange={vi.fn()}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    // The fresh agent has no offline badge.
    expect(screen.queryAllByLabelText(/offline/i)).toHaveLength(1);
    const badge = screen.getByLabelText(/offline/i);
    expect(badge).toHaveAttribute(
      "title",
      "This agent is currently offline — its pairs will be skipped after 3 attempts.",
    );
  });

  test("renders soft 'stale' badge between the stale and offline thresholds", () => {
    // 30 s ago is past the 20 s default stale threshold but well within
    // the 5 min offline threshold. A snapshot lag of up to
    // `refresh_interval_seconds` must surface as a soft "Stale" badge,
    // not a destructive "Offline" badge — the agent is still well
    // inside its active window, the registry just hasn't refreshed yet.
    const recentlyStale: AgentSummary = {
      id: "stale-soft",
      display_name: "Recently Stale",
      ip: "10.0.0.5",
      last_seen_at: new Date(Date.now() - 30_000).toISOString(),
      registered_at: "2026-01-01T00:00:00Z",
      catalogue_coordinates: null,
    };
    mockAgents([recentlyStale]);

    renderWithQuery(
      <SourcePanel
        selected={new Set()}
        onSelectedChange={vi.fn()}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    // Soft state — the badge is labelled "Stale: <id>", NOT offline.
    expect(screen.queryByLabelText(/offline/i)).not.toBeInTheDocument();
    const badge = screen.getByLabelText(`Stale: stale-soft`);
    expect(badge).toBeInTheDocument();
    expect(badge).toHaveAttribute(
      "title",
      expect.stringContaining("snapshot may be one refresh tick behind"),
    );
  });

  test("treats a fresh push as online even when refetch lag would have flipped to offline", () => {
    // A sub-second-old `last_seen_at` must render as Online regardless
    // of when the `useAgents` query last refetched. The badge samples
    // `Date.now()` at render time, so a snapshot lag of up to
    // `refresh_interval_seconds` cannot flip a freshly-pushed agent
    // through the offline threshold.
    const justPushed: AgentSummary = {
      ...AGENT_BERLIN,
      id: "fresh-push",
      display_name: "Just Pushed",
      last_seen_at: new Date(Date.now() - 500).toISOString(),
    };
    mockAgents([justPushed]);

    renderWithQuery(
      <SourcePanel
        selected={new Set()}
        onSelectedChange={vi.fn()}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    expect(screen.queryByLabelText(/offline/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/stale/i)).not.toBeInTheDocument();
    expect(screen.getByText("Online")).toBeInTheDocument();
  });

  test("clock tick transitions Online → Stale across the threshold without a data refresh", () => {
    // The `useAgents` query refetches every 30 s but the default stale
    // threshold is 20 s. Without an internal interval, an agent that
    // stops responding would stay Online for up to 10 s past the
    // threshold. The component owns a 10 s tick that drives a re-render
    // against a fresh `Date.now()`; assert the badge transitions.
    vi.useFakeTimers();
    try {
      const startMs = Date.parse("2026-04-22T12:00:00Z");
      vi.setSystemTime(startMs);

      // Agent pushed 5 s before the test starts — comfortably online.
      const recent: AgentSummary = {
        id: "ticking-agent",
        display_name: "Ticking",
        ip: "10.0.0.99",
        last_seen_at: new Date(startMs - 5_000).toISOString(),
        registered_at: "2026-01-01T00:00:00Z",
        catalogue_coordinates: null,
      };
      mockAgents([recent]);

      renderWithQuery(
        <SourcePanel
          selected={new Set()}
          onSelectedChange={vi.fn()}
          filter={EMPTY_FILTER}
          onFilterChange={vi.fn()}
          facets={undefined}
          onOpenMap={vi.fn()}
        />,
      );

      expect(screen.getByLabelText("Online: ticking-agent")).toBeInTheDocument();
      expect(screen.queryByLabelText(/stale/i)).not.toBeInTheDocument();

      // Advance the wall clock past the 20 s default stale threshold.
      // The age is now ~25 s with no data mutation; the component's
      // own setInterval drives the re-render that flips the badge.
      act(() => {
        vi.advanceTimersByTime(20_000);
      });

      expect(screen.queryByLabelText("Online: ticking-agent")).not.toBeInTheDocument();
      expect(screen.getByLabelText("Stale: ticking-agent")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  test("excludes agents with null catalogue_coordinates when a shape filter is active", () => {
    mockAgents([AGENT_BERLIN, AGENT_NO_COORDS]);

    // Rectangle containing only Berlin.
    const shapeFilter: FilterValue = {
      ...EMPTY_FILTER,
      shapes: [{ kind: "rectangle", sw: [10, 50], ne: [15, 55] }],
    };

    const { rerender } = renderWithQuery(
      <SourcePanel
        selected={new Set()}
        onSelectedChange={vi.fn()}
        filter={shapeFilter}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    expect(screen.getByText("Berlin Alpha")).toBeInTheDocument();
    expect(screen.queryByText("No Coords")).not.toBeInTheDocument();

    // Without the shape filter, the no-coords agent is included again.
    rerender(
      <SourcePanel
        selected={new Set()}
        onSelectedChange={vi.fn()}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );
    expect(screen.getByText("No Coords")).toBeInTheDocument();
  });

  describe("catalogue-joined fields", () => {
    test("renders city, country (resolved via lookup), ASN, and network_operator when populated", () => {
      const AGENT_ENRICHED: AgentSummary = {
        id: "enriched-1",
        display_name: "Enriched Agent",
        ip: "10.0.0.10",
        last_seen_at: FUTURE,
        registered_at: "2026-01-01T00:00:00Z",
        catalogue_coordinates: null,
        city: "Amsterdam",
        country_code: "NL",
        country_name: "Netherlands",
        asn: 64500,
        network_operator: "ExampleNet",
      };
      mockAgents([AGENT_ENRICHED]);

      renderPanel(
        <SourcePanel
          selected={new Set()}
          onSelectedChange={vi.fn()}
          filter={EMPTY_FILTER}
          onFilterChange={vi.fn()}
          facets={undefined}
          onOpenMap={vi.fn()}
        />,
      );

      expect(screen.getByText("Amsterdam")).toBeInTheDocument();
      // `lookupCountryName` resolves "NL" → "Netherlands"; matches the
      // label shown by the destination panel for consistency.
      expect(screen.getByText("Netherlands")).toBeInTheDocument();
      expect(screen.getByText("64500")).toBeInTheDocument();
      expect(screen.getByText("ExampleNet")).toBeInTheDocument();
    });

    test("renders em-dash placeholders when catalogue-joined fields are null", () => {
      const AGENT_BARE: AgentSummary = {
        id: "bare-1",
        display_name: "Bare Agent",
        ip: "10.0.0.11",
        last_seen_at: FUTURE,
        registered_at: "2026-01-01T00:00:00Z",
        catalogue_coordinates: null,
      };
      mockAgents([AGENT_BARE]);

      renderPanel(
        <SourcePanel
          selected={new Set()}
          onSelectedChange={vi.fn()}
          filter={EMPTY_FILTER}
          onFilterChange={vi.fn()}
          facets={undefined}
          onOpenMap={vi.fn()}
        />,
      );

      // Four null-valued cells (City, Country, ASN, Network) each render "—".
      const placeholders = screen.getAllByText("—");
      expect(placeholders.length).toBeGreaterThanOrEqual(4);
    });
  });

  describe("hostname rendering", () => {
    test("wraps the agent IP cell in <IpHostname>, appending `(hostname)` when seeded", async () => {
      mockAgents([AGENT_BERLIN]);

      renderPanel(
        <SourcePanel
          selected={new Set()}
          onSelectedChange={vi.fn()}
          filter={EMPTY_FILTER}
          onFilterChange={vi.fn()}
          facets={undefined}
          onOpenMap={vi.fn()}
        />,
        [{ ip: "10.0.0.1", hostname: "berlin.example.com" }],
      );

      expect(await screen.findByText("(berlin.example.com)")).toBeInTheDocument();
      expect(screen.getByText("10.0.0.1, hostname berlin.example.com")).toBeInTheDocument();
    });

    test("falls back to the bare IP when the provider has no hit", () => {
      mockAgents([AGENT_BERLIN]);

      renderPanel(
        <SourcePanel
          selected={new Set()}
          onSelectedChange={vi.fn()}
          filter={EMPTY_FILTER}
          onFilterChange={vi.fn()}
          facets={undefined}
          onOpenMap={vi.fn()}
        />,
      );

      expect(screen.getByText("10.0.0.1")).toBeInTheDocument();
      expect(screen.queryByText(/hostname/)).not.toBeInTheDocument();
    });
  });
});
