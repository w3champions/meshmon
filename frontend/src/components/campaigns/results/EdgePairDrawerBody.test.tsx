/**
 * EdgePairDrawerBody tests.
 *
 * Focus areas:
 * - Renders rows with CandidateRef mode="inline" for B endpoint
 * - Renders RouteLegRow for each leg
 * - Self-pair-excluded note (spec §5.5 G-5):
 *   shows when candidate is_mesh_member=true AND agent_id is in source_agent_ids
 */

import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { Campaign } from "@/api/hooks/campaigns";
import type { EdgePairsListResponse, EvaluationEdgePairDetailDto } from "@/api/hooks/evaluation";
import { IpHostnameProvider } from "@/components/ip-hostname";

// ---------------------------------------------------------------------------
// Module mocks
// ---------------------------------------------------------------------------

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return { ...actual, useEdgePairDetails: vi.fn() };
});

// Stub useAgents so the destination-agent IP lookup map is empty in tests.
vi.mock("@/api/hooks/agents", () => ({
  useAgents: () => ({ data: undefined, isLoading: false, isError: false }),
}));

// Stub CatalogueDrawerOverlay/useCatalogueDrawer so CandidateRef inline doesn't throw
vi.mock("@/components/catalogue/CatalogueDrawerOverlay", () => ({
  useCatalogueDrawer: () => ({ open: vi.fn() }),
  CatalogueDrawerOverlay: ({ children }: { children: ReactNode }) => <>{children}</>,
}));

import { useEdgePairDetails } from "@/api/hooks/evaluation";
import { EdgePairDrawerBody } from "@/components/campaigns/results/EdgePairDrawerBody";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "44444444-4444-4444-4444-444444444444";
const CANDIDATE_IP = "10.0.77.1";
const AGENT_ID = "agent-source-x";

function makeCampaign(overrides: Partial<Campaign & { source_agent_ids?: string[] }> = {}): Campaign & { source_agent_ids?: string[] } {
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
    source_agent_ids: overrides.source_agent_ids ?? [],
  } as unknown as Campaign & { source_agent_ids?: string[] };
}

function makeEdgePairRow(idx: number, overrides: Partial<EvaluationEdgePairDetailDto> = {}): EvaluationEdgePairDetailDto {
  return {
    candidate_ip: CANDIDATE_IP,
    destination_agent_id: `dest-agent-${idx}`,
    destination_hostname: null,
    best_route_ms: 20 + idx,
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

function makeEdgePairRowWithLegs(idx: number): EvaluationEdgePairDetailDto {
  return makeEdgePairRow(idx, {
    best_route_kind: "one_hop",
    best_route_legs: [
      {
        from_id: `agent-${idx}-a`,
        from_kind: "agent" as const,
        to_id: CANDIDATE_IP,
        to_kind: "candidate" as const,
        rtt_ms: 10,
        stddev_ms: 1,
        loss_ratio: 0,
        source: "active_probe" as const,
        was_substituted: false,
      },
      {
        from_id: CANDIDATE_IP,
        from_kind: "candidate" as const,
        to_id: `dest-agent-${idx}`,
        to_kind: "agent" as const,
        rtt_ms: 10,
        stddev_ms: 1,
        loss_ratio: 0,
        source: "active_probe" as const,
        was_substituted: false,
      },
    ],
  });
}

function pageOf(entries: EvaluationEdgePairDetailDto[]): EdgePairsListResponse {
  return { entries, next_cursor: null, total: entries.length };
}

function makeHookReturn(
  entries: EvaluationEdgePairDetailDto[],
  overrides: Record<string, unknown> = {},
) {
  return {
    data: { pages: [pageOf(entries)], pageParams: [null] },
    isLoading: false,
    isError: false,
    isFetchingNextPage: false,
    hasNextPage: false,
    error: null,
    fetchNextPage: vi.fn(),
    refetch: vi.fn(),
    ...overrides,
  };
}

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

function renderBody(
  entries: EvaluationEdgePairDetailDto[],
  opts: {
    is_mesh_member?: boolean;
    agent_id?: string | null;
    source_agent_ids?: string[];
    hookOverrides?: Record<string, unknown>;
  } = {},
) {
  vi.mocked(useEdgePairDetails).mockReturnValue(
    makeHookReturn(entries, opts.hookOverrides ?? {}) as unknown as ReturnType<typeof useEdgePairDetails>,
  );

  const candidate = {
    destination_ip: CANDIDATE_IP,
    is_mesh_member: opts.is_mesh_member ?? false,
    agent_id: opts.agent_id ?? null,
    pairs_improved: 3,
    pairs_total_considered: 5,
  };

  const campaign = makeCampaign({ source_agent_ids: opts.source_agent_ids ?? [] });

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

  return render(
    <EdgePairDrawerBody
      candidateIp={CANDIDATE_IP}
      candidate={candidate as Parameters<typeof EdgePairDrawerBody>[0]["candidate"]}
      campaign={campaign as Campaign}
    />,
    { wrapper: Wrapper },
  );
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
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

describe("EdgePairDrawerBody — row rendering", () => {
  test("renders a row for each edge pair entry", () => {
    renderBody([makeEdgePairRow(1), makeEdgePairRow(2)]);
    expect(screen.getByTestId("edge-pair-row-0")).toBeInTheDocument();
    expect(screen.getByTestId("edge-pair-row-1")).toBeInTheDocument();
  });

  test("renders the destination agent label and id for the B endpoint", () => {
    renderBody([makeEdgePairRow(1)]);
    expect(screen.getByTestId("edge-pair-row-0")).toBeInTheDocument();
    // Destination agent id is rendered in the row (label + secondary line
    // both fall back to agent_id when no hostname / agentsById entry).
    expect(screen.getAllByText("dest-agent-1").length).toBeGreaterThanOrEqual(1);
  });

  test("renders RouteLegRow for each leg in the best route", () => {
    renderBody([makeEdgePairRowWithLegs(1)]);
    // RouteLegRow renders leg RTT
    expect(screen.getAllByText(/10\.0 ms/)).toBeTruthy();
  });

  test("renders the 'qualifies' badge when qualifies_under_t is true", () => {
    renderBody([makeEdgePairRow(1, { qualifies_under_t: true })]);
    expect(screen.getByText("qualifies")).toBeInTheDocument();
  });

  test("renders 'above T' badge when qualifies_under_t is false", () => {
    renderBody([makeEdgePairRow(1, { qualifies_under_t: false })]);
    expect(screen.getByText("above T")).toBeInTheDocument();
  });

  test("renders 'unreachable' placeholder when best_route_ms is null", () => {
    renderBody([
      makeEdgePairRow(1, {
        is_unreachable: true,
        best_route_ms: null,
        best_route_loss_ratio: 1,
        qualifies_under_t: false,
      }),
    ]);
    // Both the RTT cell and the qualifies badge surface "unreachable" for
    // a fully unreachable row — assert at least one rendered, since the
    // contract is "the cell must not crash on null best_route_ms".
    expect(screen.getAllByText("unreachable").length).toBeGreaterThanOrEqual(1);
  });
});

describe("EdgePairDrawerBody — self-pair-excluded note", () => {
  test("shows the self-pair note when candidate is mesh member AND agent_id is in source_agent_ids", () => {
    renderBody([], {
      is_mesh_member: true,
      agent_id: AGENT_ID,
      source_agent_ids: [AGENT_ID],
    });
    expect(screen.getByTestId("self-pair-excluded-note")).toBeInTheDocument();
    expect(screen.getByText(/Self-pair excluded/)).toBeInTheDocument();
    expect(screen.getByText(/also a source agent/)).toBeInTheDocument();
  });

  test("does NOT show the note when is_mesh_member is false", () => {
    renderBody([], {
      is_mesh_member: false,
      agent_id: AGENT_ID,
      source_agent_ids: [AGENT_ID],
    });
    expect(screen.queryByTestId("self-pair-excluded-note")).not.toBeInTheDocument();
  });

  test("does NOT show the note when agent_id is not in source_agent_ids", () => {
    renderBody([], {
      is_mesh_member: true,
      agent_id: AGENT_ID,
      source_agent_ids: ["other-agent"],
    });
    expect(screen.queryByTestId("self-pair-excluded-note")).not.toBeInTheDocument();
  });

  test("does NOT show the note when agent_id is null", () => {
    renderBody([], {
      is_mesh_member: true,
      agent_id: null,
      source_agent_ids: [AGENT_ID],
    });
    expect(screen.queryByTestId("self-pair-excluded-note")).not.toBeInTheDocument();
  });
});

describe("EdgePairDrawerBody — empty and loading states", () => {
  test("renders the body container", () => {
    renderBody([]);
    expect(screen.getByTestId("edge-pair-drawer-body")).toBeInTheDocument();
  });

  test("shows loading card while hook is loading", () => {
    renderBody([], {
      hookOverrides: {
        data: undefined,
        isLoading: true,
      },
    });
    expect(screen.getByTestId("edge-pair-loading")).toBeInTheDocument();
  });

  test("shows error card on hook error", () => {
    renderBody([], {
      hookOverrides: {
        data: undefined,
        isLoading: false,
        isError: true,
        error: new Error("network error"),
      },
    });
    expect(screen.getByText(/failed to load edge pair details/i)).toBeInTheDocument();
  });
});
