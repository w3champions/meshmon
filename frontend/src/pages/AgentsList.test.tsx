import { screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import * as agentsHook from "@/api/hooks/agents";
import AgentsList from "@/pages/AgentsList";
import { renderWithProviders } from "@/test/query-wrapper";

vi.mock("@/api/hooks/agents");

// Mock date-fns so tests don't depend on wall-clock time.
vi.mock("date-fns", async (importOriginal) => {
  const actual = await importOriginal<typeof import("date-fns")>();
  return {
    ...actual,
    formatDistanceToNowStrict: (_date: Date, _opts?: unknown) => "1 minute ago",
  };
});

const AGENTS: AgentSummary[] = [
  {
    id: "alpha",
    display_name: "Alpha",
    ip: "10.0.0.1",
    location: "Frankfurt",
    agent_version: "0.1.0",
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: new Date(Date.now() + 60_000).toISOString(),
  },
  {
    id: "beta",
    display_name: "Beta",
    ip: "10.0.0.2",
    location: "Tokyo",
    agent_version: "0.1.0",
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: new Date(Date.now() + 60_000).toISOString(),
  },
];

afterEach(() => {
  vi.clearAllMocks();
});

describe("AgentsList", () => {
  test("shows skeleton when loading", async () => {
    vi.mocked(agentsHook.useAgents).mockReturnValue({
      data: undefined,
      isLoading: true,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgents>);

    renderWithProviders(<AgentsList />);

    expect(await screen.findByTestId("agents-skeleton")).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  test("shows error message on error", async () => {
    vi.mocked(agentsHook.useAgents).mockReturnValue({
      data: undefined,
      isLoading: false,
      isError: true,
    } as ReturnType<typeof agentsHook.useAgents>);

    renderWithProviders(<AgentsList />);

    expect(await screen.findByRole("alert")).toHaveTextContent("Failed to load agents");
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  test("shows empty state message when no agents are registered", async () => {
    vi.mocked(agentsHook.useAgents).mockReturnValue({
      data: [] as AgentSummary[],
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgents>);

    renderWithProviders(<AgentsList />);

    expect(await screen.findByText("No agents registered yet")).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  test("renders table with filter input and rows when data is present", async () => {
    vi.mocked(agentsHook.useAgents).mockReturnValue({
      data: AGENTS,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgents>);

    renderWithProviders(<AgentsList />);

    // Filter input should be present
    expect(await screen.findByRole("textbox", { name: /filter agents/i })).toBeInTheDocument();

    // Table should be rendered
    expect(screen.getByRole("table")).toBeInTheDocument();

    // Agent rows should be visible
    expect(screen.getByText("alpha")).toBeInTheDocument();
    expect(screen.getByText("beta")).toBeInTheDocument();
  });

  test("renders page heading", async () => {
    vi.mocked(agentsHook.useAgents).mockReturnValue({
      data: AGENTS,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof agentsHook.useAgents>);

    renderWithProviders(<AgentsList />);

    expect(await screen.findByRole("heading", { name: "Agents" })).toBeInTheDocument();
  });
});
