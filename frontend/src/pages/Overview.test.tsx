import { screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import * as agentsHook from "@/api/hooks/agents";
import * as alertsHook from "@/api/hooks/alerts";
import type { HealthMatrix } from "@/api/hooks/health-matrix";
import * as healthMatrixHook from "@/api/hooks/health-matrix";
import type { RouteSnapshotSummary } from "@/api/hooks/recent-routes";
import * as recentRoutesHook from "@/api/hooks/recent-routes";
import Overview from "@/pages/Overview";
import { renderWithProviders } from "@/test/query-wrapper";

// Mock react-leaflet using the shared leaflet mock so the map renders to divs.
vi.mock("react-leaflet", async () => {
  const { LeafletMock } = await import("@/test/leaflet-mock");
  return LeafletMock;
});

vi.mock("@/api/hooks/agents");
vi.mock("@/api/hooks/alerts");
vi.mock("@/api/hooks/health-matrix");
vi.mock("@/api/hooks/recent-routes");

// Mock date-fns so tests are deterministic (same strategy as RecentRoutesTable.test.tsx).
vi.mock("date-fns", async (importOriginal) => {
  const actual = await importOriginal<typeof import("date-fns")>();
  return {
    ...actual,
    formatDistanceToNowStrict: (date: Date, _opts?: unknown) => {
      const iso = date instanceof Date ? date.toISOString() : String(date);
      if (iso.includes("11:59")) return "1 minute ago";
      if (iso.includes("11:58")) return "2 minutes ago";
      return "some time ago";
    },
  };
});

const AGENTS: AgentSummary[] = [
  {
    id: "a",
    display_name: "A",
    ip: "10.0.0.1",
    lat: 48.14,
    lon: 11.58,
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: "2026-04-16T11:59:00Z",
  },
  {
    id: "b",
    display_name: "B",
    ip: "10.0.0.2",
    lat: 51.51,
    lon: -0.13,
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: "2026-04-16T11:59:00Z",
  },
];

const MATRIX: HealthMatrix = new Map([
  ["a>b", { source: "a", target: "b", failureRate: 0.01, state: "normal" }],
  ["b>a", { source: "b", target: "a", failureRate: 0.01, state: "normal" }],
]);

const ROUTE_ROWS = [
  {
    id: 1,
    source_id: "a",
    target_id: "b",
    protocol: "icmp",
    observed_at: "2026-04-16T11:59:00Z",
    path_summary: null,
  },
  {
    id: 2,
    source_id: "b",
    target_id: "a",
    protocol: "udp",
    observed_at: "2026-04-16T11:58:00Z",
    path_summary: null,
  },
];

function setupPopulatedMocks() {
  vi.mocked(agentsHook.useAgents).mockReturnValue({
    data: AGENTS,
    isLoading: false,
    isError: false,
  } as ReturnType<typeof agentsHook.useAgents>);

  vi.mocked(healthMatrixHook.useHealthMatrix).mockReturnValue({
    data: MATRIX,
    isLoading: false,
    isError: false,
  } as ReturnType<typeof healthMatrixHook.useHealthMatrix>);

  vi.mocked(alertsHook.useAlertSummary).mockReturnValue({
    data: { critical: 1, warning: 2, info: 0, total: 3 },
    isLoading: false,
    isError: false,
  });

  vi.mocked(recentRoutesHook.useRecentRouteChanges).mockReturnValue({
    data: ROUTE_ROWS,
    isLoading: false,
    isError: false,
  } as ReturnType<typeof recentRoutesHook.useRecentRouteChanges>);
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("Overview", () => {
  describe("populated state", () => {
    test("renders the AgentMap shell", async () => {
      setupPopulatedMocks();
      renderWithProviders(<Overview />);
      expect(await screen.findByTestId("agent-map-shell")).toBeInTheDocument();
    });

    test("renders PathHealthGrid cells with data-state", async () => {
      setupPopulatedMocks();
      renderWithProviders(<Overview />);
      // PathHealthGrid renders gridcells. Wait for any gridcell to appear, then
      // verify that interactive (non-self) cells carry data-state.
      const allCells = await screen.findAllByRole("gridcell");
      expect(allCells.length).toBeGreaterThan(0);
      const interactiveCells = allCells.filter((cell) => cell.hasAttribute("href"));
      expect(interactiveCells.length).toBeGreaterThan(0);
      for (const cell of interactiveCells) {
        expect(cell).toHaveAttribute("data-state");
      }
    });

    test("renders AlertSummaryStrip with View all link", async () => {
      setupPopulatedMocks();
      renderWithProviders(<Overview />);
      expect(await screen.findByText("View all")).toBeInTheDocument();
    });

    test("renders AlertSummaryStrip with critical badge", async () => {
      setupPopulatedMocks();
      renderWithProviders(<Overview />);
      expect(await screen.findByText("1 critical")).toBeInTheDocument();
      expect(screen.getByText("2 warning")).toBeInTheDocument();
    });

    test("renders RecentRoutesTable rows", async () => {
      setupPopulatedMocks();
      renderWithProviders(<Overview />);
      expect(await screen.findByText("a → b")).toBeInTheDocument();
      expect(screen.getByText("b → a")).toBeInTheDocument();
    });
  });

  describe("loading state", () => {
    test("shows map and grid skeletons when useAgents is loading", async () => {
      vi.mocked(agentsHook.useAgents).mockReturnValue({
        data: undefined,
        isLoading: true,
        isError: false,
      } as ReturnType<typeof agentsHook.useAgents>);

      vi.mocked(healthMatrixHook.useHealthMatrix).mockReturnValue({
        data: undefined,
        isLoading: true,
        isError: false,
      } as ReturnType<typeof healthMatrixHook.useHealthMatrix>);

      vi.mocked(alertsHook.useAlertSummary).mockReturnValue({
        data: { critical: 0, warning: 0, info: 0, total: 0 },
        isLoading: true,
        isError: false,
      });

      vi.mocked(recentRoutesHook.useRecentRouteChanges).mockReturnValue({
        data: undefined,
        isLoading: true,
        isError: false,
      } as ReturnType<typeof recentRoutesHook.useRecentRouteChanges>);

      renderWithProviders(<Overview />);

      expect(await screen.findByTestId("map-skeleton")).toBeInTheDocument();
      expect(screen.getByTestId("grid-skeleton")).toBeInTheDocument();
      // Map and grid should not be rendered
      expect(screen.queryByTestId("agent-map-shell")).not.toBeInTheDocument();
    });
  });

  describe("empty state", () => {
    test("propagates empty agents to PathHealthGrid empty state", async () => {
      vi.mocked(agentsHook.useAgents).mockReturnValue({
        data: [] as AgentSummary[],
        isLoading: false,
        isError: false,
      } as ReturnType<typeof agentsHook.useAgents>);

      vi.mocked(healthMatrixHook.useHealthMatrix).mockReturnValue({
        data: new Map(),
        isLoading: false,
        isError: false,
      } as ReturnType<typeof healthMatrixHook.useHealthMatrix>);

      vi.mocked(alertsHook.useAlertSummary).mockReturnValue({
        data: { critical: 0, warning: 0, info: 0, total: 0 },
        isLoading: false,
        isError: false,
      });

      vi.mocked(recentRoutesHook.useRecentRouteChanges).mockReturnValue({
        data: [] as RouteSnapshotSummary[],
        isLoading: false,
        isError: false,
      } as ReturnType<typeof recentRoutesHook.useRecentRouteChanges>);

      renderWithProviders(<Overview />);

      // PathHealthGrid shows its own empty state when agents is empty.
      expect(await screen.findByText("No agents registered yet.")).toBeInTheDocument();
      // RecentRoutesTable is always rendered; with empty routes it shows its own
      // "No recent route changes" paragraph — not a <table> element.
      expect(await screen.findByText("No recent route changes")).toBeInTheDocument();
      expect(screen.queryByRole("table")).not.toBeInTheDocument();
    });
  });
});
