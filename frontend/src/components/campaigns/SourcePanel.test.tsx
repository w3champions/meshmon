import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import * as agentsHook from "@/api/hooks/agents";
import { SourcePanel } from "@/components/campaigns/SourcePanel";
import type { FilterValue } from "@/components/filter/FilterRail";
import { renderWithQuery } from "@/test/query-wrapper";

vi.mock("@/api/hooks/agents");

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
}

afterEach(() => {
  vi.clearAllMocks();
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
});
