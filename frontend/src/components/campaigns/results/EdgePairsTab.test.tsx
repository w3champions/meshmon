/**
 * EdgePairsTab tests.
 *
 * Focus areas:
 * - The destination cell resolves the agent via `useAgents()` and renders
 *   the agent's IP / display label rather than the raw `destination_agent_id`.
 * - Clicking the destination CandidateRef inline trigger calls the catalogue
 *   drawer with the agent's IP — not the agent_id.
 * - Falls back to the agent_id label when the agent is missing from the
 *   roster (so the cell still renders something instead of crashing).
 */

import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { Campaign } from "@/api/hooks/campaigns";
import type {
  EdgePairsListResponse,
  EvaluationEdgePairDetailDto,
} from "@/api/hooks/evaluation";
import { IpHostnameProvider } from "@/components/ip-hostname";

// ---------------------------------------------------------------------------
// Module mocks
// ---------------------------------------------------------------------------

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return { ...actual, useEdgePairDetails: vi.fn() };
});

vi.mock("@/api/hooks/agents", () => ({
  useAgents: vi.fn(),
}));

const mockOpen = vi.fn();
vi.mock("@/components/catalogue/CatalogueDrawerOverlay", () => ({
  useCatalogueDrawer: () => ({ open: mockOpen }),
  CatalogueDrawerOverlay: ({ children }: { children: ReactNode }) => <>{children}</>,
}));

vi.mock("@tanstack/react-router", async () => {
  const actual =
    await vi.importActual<typeof import("@tanstack/react-router")>("@tanstack/react-router");
  return {
    ...actual,
    useNavigate: () => vi.fn(),
    useSearch: () => ({}),
  };
});

import { useAgents } from "@/api/hooks/agents";
import { useEdgePairDetails } from "@/api/hooks/evaluation";
import { EdgePairsTab } from "@/components/campaigns/results/EdgePairsTab";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "55555555-5555-5555-5555-555555555555";
const CANDIDATE_IP = "10.0.55.1";
const DEST_AGENT_ID = "agent-dest-7";
const DEST_AGENT_IP = "10.0.55.42";
const DEST_AGENT_DISPLAY = "frankfurt-7";

function makeCampaign(): Campaign {
  return {
    id: CAMPAIGN_ID,
    title: "edge campaign",
    notes: "",
    state: "evaluated",
    protocol: "icmp",
    evaluation_mode: "edge_candidate",
    force_measurement: false,
    loss_threshold_ratio: 0.02,
    stddev_weight: 1,
    probe_count: 10,
    probe_count_detail: 250,
    probe_stagger_ms: 100,
    timeout_ms: 2000,
    created_at: "2026-04-01T12:00:00Z",
    created_by: "alice",
    started_at: null,
    stopped_at: null,
    completed_at: null,
    evaluated_at: null,
    pair_counts: [],
  } as unknown as Campaign;
}

function makeEdgePairRow(
  overrides: Partial<EvaluationEdgePairDetailDto> = {},
): EvaluationEdgePairDetailDto {
  return {
    candidate_ip: CANDIDATE_IP,
    destination_agent_id: DEST_AGENT_ID,
    destination_hostname: null,
    best_route_ms: 25,
    best_route_loss_ratio: 0,
    best_route_stddev_ms: 1,
    best_route_kind: "direct",
    best_route_legs: [],
    best_route_intermediaries: [],
    qualifies_under_t: true,
    is_unreachable: false,
    ...overrides,
  };
}

function makeAgent(overrides: Partial<AgentSummary> = {}): AgentSummary {
  return {
    id: DEST_AGENT_ID,
    ip: DEST_AGENT_IP,
    display_name: DEST_AGENT_DISPLAY,
    last_seen_at: "2026-04-01T12:00:00Z",
    ...overrides,
  } as unknown as AgentSummary;
}

function pageOf(entries: EvaluationEdgePairDetailDto[]): EdgePairsListResponse {
  return { entries, next_cursor: null, total: entries.length };
}

function makeHookReturn(entries: EvaluationEdgePairDetailDto[]) {
  return {
    data: { pages: [pageOf(entries)], pageParams: [null] },
    isLoading: false,
    isError: false,
    isFetchingNextPage: false,
    hasNextPage: false,
    error: null,
    fetchNextPage: vi.fn(),
    refetch: vi.fn(),
  };
}

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

function renderTab(
  entries: EvaluationEdgePairDetailDto[],
  agents: AgentSummary[] = [],
) {
  vi.mocked(useEdgePairDetails).mockReturnValue(
    makeHookReturn(entries) as unknown as ReturnType<typeof useEdgePairDetails>,
  );
  vi.mocked(useAgents).mockReturnValue({
    data: agents,
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useAgents>);

  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });

  function Wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={client}>
        <IpHostnameProvider>{children}</IpHostnameProvider>
      </QueryClientProvider>
    );
  }

  return render(<EdgePairsTab campaign={makeCampaign()} />, { wrapper: Wrapper });
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
  mockOpen.mockReset();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("EdgePairsTab — destination cell rendering", () => {
  test("resolves the destination agent through useAgents() and renders its IP", () => {
    renderTab([makeEdgePairRow()], [makeAgent()]);
    // The agent's IP is rendered as the secondary line under the B label
    // (the row also includes the X candidate IP, hence the multi-match).
    expect(screen.getAllByText(DEST_AGENT_IP).length).toBeGreaterThanOrEqual(1);
    // The CandidateRef inline trigger renders the agent's display_name
    // (preferred when destination_hostname is absent).
    expect(screen.getByText(DEST_AGENT_DISPLAY)).toBeInTheDocument();
  });

  test("clicking the destination CandidateRef opens the catalogue drawer with the agent's IP", async () => {
    const user = userEvent.setup();
    renderTab([makeEdgePairRow()], [makeAgent()]);

    const trigger = screen.getByRole("button", { name: DEST_AGENT_DISPLAY });
    await user.click(trigger);

    expect(mockOpen).toHaveBeenCalledWith(DEST_AGENT_IP);
    expect(mockOpen).not.toHaveBeenCalledWith(DEST_AGENT_ID);
  });

  test("prefers destination_hostname over the agent's display_name when present", () => {
    renderTab(
      [makeEdgePairRow({ destination_hostname: "frankfurt-7.dc.example" })],
      [makeAgent()],
    );
    expect(screen.getByText("frankfurt-7.dc.example")).toBeInTheDocument();
  });

  test("falls back to the agent_id label when the agent is missing from the roster", () => {
    renderTab([makeEdgePairRow()], []);
    // No CandidateRef inline — the cell renders a plain label and the
    // agent_id as the secondary line.
    expect(screen.getAllByText(DEST_AGENT_ID).length).toBeGreaterThanOrEqual(1);
    // No catalogue drawer trigger button rendered for the missing agent.
    expect(
      screen.queryByRole("button", { name: DEST_AGENT_ID }),
    ).not.toBeInTheDocument();
  });
});
