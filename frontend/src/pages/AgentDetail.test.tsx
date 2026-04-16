import { screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import * as agentsHook from "@/api/hooks/agents";
import type { HealthMatrix } from "@/api/hooks/health-matrix";
import * as healthMatrixHook from "@/api/hooks/health-matrix";
import AgentDetail from "@/pages/AgentDetail";
import { renderWithProviders } from "@/test/query-wrapper";

// Option B: mock the router module so agentDetailRoute.useParams() returns a
// controlled id without requiring the real app router tree to be mounted.
vi.mock("@/router/index", () => ({
  agentDetailRoute: {
    useParams: () => ({ id: "agent-a" }),
  },
}));

vi.mock("@/api/hooks/agents");
vi.mock("@/api/hooks/health-matrix");

// Mock date-fns so tests are deterministic.
vi.mock("date-fns", async (importOriginal) => {
  const actual = await importOriginal<typeof import("date-fns")>();
  return {
    ...actual,
    formatDistanceToNowStrict: (_date: Date, _opts?: unknown) => "1 minute ago",
  };
});

const AGENT_A: AgentSummary = {
  id: "agent-a",
  display_name: "Agent Alpha",
  ip: "10.0.0.1",
  location: "Frankfurt",
  agent_version: "0.1.0",
  registered_at: "2026-01-01T00:00:00Z",
  last_seen_at: new Date(Date.now() + 60_000).toISOString(),
};

const AGENT_B: AgentSummary = {
  id: "agent-b",
  display_name: "Agent Beta",
  ip: "10.0.0.2",
  location: "Tokyo",
  agent_version: "0.1.0",
  registered_at: "2026-01-01T00:00:00Z",
  last_seen_at: new Date(Date.now() + 60_000).toISOString(),
};

const ALL_AGENTS: AgentSummary[] = [AGENT_A, AGENT_B];

const MATRIX: HealthMatrix = new Map([
  ["agent-a>agent-b", { source: "agent-a", target: "agent-b", failureRate: 0.01, state: "normal" }],
  [
    "agent-b>agent-a",
    { source: "agent-b", target: "agent-a", failureRate: 0.05, state: "degraded" },
  ],
]);

afterEach(() => {
  vi.clearAllMocks();
});

describe("AgentDetail", () => {
  test("shows skeleton while loading", async () => {
    vi.mocked(agentsHook.useAgent).mockReturnValue({
      data: undefined,
      isLoading: true,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgent>);

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

    renderWithProviders(<AgentDetail />);

    expect(await screen.findByTestId("agent-detail-skeleton")).toBeInTheDocument();
  });

  test("shows 'Agent not found' with a back link when agent is null", async () => {
    vi.mocked(agentsHook.useAgent).mockReturnValue({
      data: null,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgent>);

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

    renderWithProviders(<AgentDetail />);

    expect(await screen.findByText("Agent not found")).toBeInTheDocument();
    const backLink = screen.getByRole("link", { name: /back to agents/i });
    expect(backLink).toBeInTheDocument();
    expect(backLink).toHaveAttribute("href", "/agents");
  });

  test("renders AgentCard with the agent's name and id", async () => {
    vi.mocked(agentsHook.useAgent).mockReturnValue({
      data: AGENT_A,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgent>);

    vi.mocked(agentsHook.useAgents).mockReturnValue({
      data: ALL_AGENTS,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgents>);

    vi.mocked(healthMatrixHook.useHealthMatrix).mockReturnValue({
      data: MATRIX,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof healthMatrixHook.useHealthMatrix>);

    renderWithProviders(<AgentDetail />);

    // AgentCard renders display_name as CardTitle and id as CardDescription
    expect(await screen.findByText("Agent Alpha")).toBeInTheDocument();
    // "agent-a" appears in multiple places (AgentCard + grid row headers) — just verify at least one exists
    expect(screen.getAllByText("agent-a").length).toBeGreaterThanOrEqual(1);
  });

  test("renders 'Outgoing paths' PathHealthGrid with sourceFilter === agent.id", async () => {
    vi.mocked(agentsHook.useAgent).mockReturnValue({
      data: AGENT_A,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgent>);

    vi.mocked(agentsHook.useAgents).mockReturnValue({
      data: ALL_AGENTS,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgents>);

    vi.mocked(healthMatrixHook.useHealthMatrix).mockReturnValue({
      data: MATRIX,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof healthMatrixHook.useHealthMatrix>);

    renderWithProviders(<AgentDetail />);

    expect(await screen.findByText("Outgoing paths")).toBeInTheDocument();

    // PathHealthGrid with sourceFilter=agent-a:
    //   rows = ["agent-a"] (sourceFilter applied), cols = ["agent-a", "agent-b"]
    //   → 1 row-header rendered (agent-a)
    // PathHealthGrid with targetFilter=agent-a:
    //   rows = ["agent-a", "agent-b"] (no sourceFilter), cols = ["agent-a"] (targetFilter applied)
    //   → 2 row-headers rendered (agent-a, agent-b)
    // Total row headers = 3
    const rowHeaders = screen.getAllByTestId("row-header");
    expect(rowHeaders).toHaveLength(3);
    // First row header belongs to the outgoing grid (sourceFilter=agent-a → only agent-a row)
    expect(rowHeaders[0]).toHaveTextContent("agent-a");
    // Next two belong to the incoming grid (all agents as rows, targetFilter=agent-a col)
    expect(rowHeaders[1]).toHaveTextContent("agent-a");
    expect(rowHeaders[2]).toHaveTextContent("agent-b");
  });

  test("renders 'Incoming paths' section heading", async () => {
    vi.mocked(agentsHook.useAgent).mockReturnValue({
      data: AGENT_A,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgent>);

    vi.mocked(agentsHook.useAgents).mockReturnValue({
      data: ALL_AGENTS,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgents>);

    vi.mocked(healthMatrixHook.useHealthMatrix).mockReturnValue({
      data: MATRIX,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof healthMatrixHook.useHealthMatrix>);

    renderWithProviders(<AgentDetail />);

    expect(await screen.findByText("Incoming paths")).toBeInTheDocument();
  });
});
