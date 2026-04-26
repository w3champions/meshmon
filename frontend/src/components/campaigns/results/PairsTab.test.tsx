import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster, toast } from "sonner";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { Campaign, CampaignPair, CampaignState } from "@/api/hooks/campaigns";
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

vi.mock("@/api/hooks/agents", async () => {
  const actual = await vi.importActual<typeof import("@/api/hooks/agents")>("@/api/hooks/agents");
  return { ...actual, useAgents: vi.fn() };
});

vi.mock("@/api/hooks/campaigns", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/campaigns")>("@/api/hooks/campaigns");
  return {
    ...actual,
    useCampaignPairs: vi.fn(),
    useForcePair: vi.fn(),
  };
});

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return { ...actual, useTriggerDetail: vi.fn(), useEdgePairDetails: vi.fn() };
});

vi.mock("@tanstack/react-router", async () => {
  const actual =
    await vi.importActual<typeof import("@tanstack/react-router")>("@tanstack/react-router");
  return {
    ...actual,
    useNavigate: () => vi.fn(),
    useSearch: () => ({}),
  };
});

vi.mock("@/components/catalogue/CatalogueDrawerOverlay", () => {
  return {
    CatalogueDrawerOverlay: ({ children }: { children: React.ReactNode }) => children,
    useCatalogueDrawer: () => ({ open: vi.fn(), close: vi.fn(), isOpen: false, entryId: null }),
  };
});

import { useAgents } from "@/api/hooks/agents";
import { useCampaignPairs, useForcePair } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";
import { useEdgePairDetails, useTriggerDetail } from "@/api/hooks/evaluation";
import { PairsTab } from "@/components/campaigns/results/PairsTab";

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

function makePair(overrides: Partial<CampaignPair> & { id: number }): CampaignPair {
  return {
    id: overrides.id,
    campaign_id: overrides.campaign_id ?? CAMPAIGN_ID,
    source_agent_id: overrides.source_agent_id ?? "agent-a",
    destination_ip: overrides.destination_ip ?? "10.0.0.1",
    resolution_state: overrides.resolution_state ?? "succeeded",
    measurement_id: overrides.measurement_id ?? 1,
    attempt_count: overrides.attempt_count ?? 1,
    dispatched_at: overrides.dispatched_at ?? "2026-04-21T10:00:00Z",
    settled_at: overrides.settled_at ?? "2026-04-21T10:01:00Z",
    last_error: overrides.last_error ?? null,
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

interface MutationStub {
  mutate: ReturnType<typeof vi.fn>;
  mutateAsync: ReturnType<typeof vi.fn>;
  isPending: boolean;
  reset: ReturnType<typeof vi.fn>;
}

function makeMutationStub(): MutationStub {
  return { mutate: vi.fn(), mutateAsync: vi.fn(), isPending: false, reset: vi.fn() };
}

const forcePairStub = makeMutationStub();
const triggerDetailStub = makeMutationStub();

function makeEdgePairRow(
  candidateIp: string,
  destinationAgentId: string,
  overrides?: Partial<{
    best_route_ms: number;
    best_route_kind: "direct" | "one_hop" | "two_hop";
    best_route_loss_ratio: number;
    best_route_stddev_ms: number;
    qualifies_under_t: boolean;
    is_unreachable: boolean;
  }>,
) {
  return {
    candidate_ip: candidateIp,
    destination_agent_id: destinationAgentId,
    best_route_ms: overrides?.best_route_ms ?? 25.5,
    best_route_kind: overrides?.best_route_kind ?? "direct",
    best_route_legs: [],
    best_route_intermediaries: [],
    best_route_loss_ratio: overrides?.best_route_loss_ratio ?? 0,
    best_route_stddev_ms: overrides?.best_route_stddev_ms ?? 1.2,
    qualifies_under_t: overrides?.qualifies_under_t ?? true,
    is_unreachable: overrides?.is_unreachable ?? false,
  };
}

function setupMocks(pairs: CampaignPair[], opts?: { isLoading?: boolean; isError?: boolean }) {
  vi.mocked(useCampaignPairs).mockReturnValue({
    data: pairs,
    isLoading: opts?.isLoading ?? false,
    isError: opts?.isError ?? false,
    error: opts?.isError ? new Error("boom") : null,
  } as unknown as ReturnType<typeof useCampaignPairs>);

  vi.mocked(useAgents).mockReturnValue({
    data: [makeAgent("agent-a", "alpha", "10.0.0.101"), makeAgent("agent-b", "beta", "10.0.0.102")],
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useAgents>);

  vi.mocked(useForcePair).mockReturnValue(
    forcePairStub as unknown as ReturnType<typeof useForcePair>,
  );
  vi.mocked(useTriggerDetail).mockReturnValue(
    triggerDetailStub as unknown as ReturnType<typeof useTriggerDetail>,
  );

  vi.mocked(useEdgePairDetails).mockReturnValue({
    data: { pages: [], pageParams: [] },
    isLoading: false,
    isError: false,
    error: null,
    hasNextPage: false,
    isFetchingNextPage: false,
    fetchNextPage: vi.fn(),
    refetch: vi.fn(),
  } as unknown as ReturnType<typeof useEdgePairDetails>);
}

function makeEvaluation(): Evaluation {
  return {
    campaign_id: CAMPAIGN_ID,
    evaluated_at: "2026-04-21T10:00:00Z",
    evaluation_mode: "edge_candidate",
    loss_threshold_ratio: 0.02,
    stddev_weight: 1,
    max_hops: 2,
    vm_lookback_minutes: 15,
    baseline_pair_count: 0,
    candidates_total: 0,
    candidates_good: 0,
    avg_improvement_ms: null,
    results: { candidates: [], unqualified_reasons: {} },
  } as unknown as Evaluation;
}

function renderTab(campaign: Campaign, evaluation: Evaluation | null = makeEvaluation()) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <IpHostnameProvider>
        <PairsTab campaign={campaign} evaluation={evaluation} />
        <Toaster />
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
  forcePairStub.mutate.mockReset();
  triggerDetailStub.mutate.mockReset();
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

describe("PairsTab — hook wiring", () => {
  test("requests the full pair cap so large campaigns don't silently truncate", () => {
    // Backend default is 500 rows; without an explicit `limit` the tab
    // would quietly drop everything past 500 on larger campaigns. Pin
    // the exact request so a future refactor can't silently regress.
    setupMocks([]);
    renderTab(makeCampaign({ state: "running" }));
    expect(useCampaignPairs).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({ limit: 5000 }),
    );
  });
});

describe("PairsTab — loading + error + empty", () => {
  test("renders the skeleton while pairs are loading", () => {
    setupMocks([], { isLoading: true });
    renderTab(makeCampaign({ state: "running" }));
    expect(screen.getByTestId("pairs-tab")).toHaveAttribute("role", "status");
  });

  test("renders the error card when the pairs fetch fails", () => {
    setupMocks([], { isError: true });
    renderTab(makeCampaign({ state: "running" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/failed to load pairs/i);
  });

  test("renders the empty state when the pair list is empty", () => {
    setupMocks([]);
    renderTab(makeCampaign({ state: "completed" }));
    expect(screen.getByText(/no pairs on this campaign yet/i)).toBeInTheDocument();
  });
});

describe("PairsTab — happy path", () => {
  test("renders one row per pair with resolution state and source display name", () => {
    const pairs = [
      makePair({ id: 1, source_agent_id: "agent-a", destination_ip: "10.0.0.1" }),
      makePair({
        id: 2,
        source_agent_id: "agent-b",
        destination_ip: "10.0.0.2",
        resolution_state: "pending",
        dispatched_at: null,
        settled_at: null,
      }),
    ];
    setupMocks(pairs);
    renderTab(makeCampaign({ state: "running" }));

    expect(screen.getByTestId("pair-row-1")).toBeInTheDocument();
    expect(screen.getByTestId("pair-row-2")).toBeInTheDocument();

    // Agent display name surfaces for pair 1's source.
    const row1 = screen.getByTestId("pair-row-1");
    expect(row1).toHaveTextContent(/alpha/);
    // Pending pair surfaces "—" for the missing timestamps.
    const row2 = screen.getByTestId("pair-row-2");
    expect(row2).toHaveTextContent(/pending/);
  });

  test("state column sorts by lifecycle order, not alphabetical", async () => {
    // Lifecycle order (asc): pending → dispatched → reused → succeeded
    // → unreachable → skipped. Lexicographic comparison would place
    // `dispatched < pending < reused < skipped < succeeded < unreachable`,
    // which doesn't match how operators scan the queue.
    const pairs = [
      makePair({ id: 1, destination_ip: "10.0.0.1", resolution_state: "succeeded" }),
      makePair({ id: 2, destination_ip: "10.0.0.2", resolution_state: "pending" }),
      makePair({ id: 3, destination_ip: "10.0.0.3", resolution_state: "skipped" }),
      makePair({ id: 4, destination_ip: "10.0.0.4", resolution_state: "dispatched" }),
    ];
    setupMocks(pairs);
    renderTab(makeCampaign({ state: "running" }));

    // Default sort is `state asc` — confirm the lifecycle ordering is
    // applied from first render (no header click needed).
    const rowOrder = screen
      .getAllByTestId(/pair-row-\d/)
      .map((row) => row.getAttribute("data-pair-destination"));
    expect(rowOrder).toEqual(["10.0.0.2", "10.0.0.4", "10.0.0.1", "10.0.0.3"]);
  });

  test("sort cycles header between desc and asc", async () => {
    const pairs = [
      makePair({ id: 1, destination_ip: "10.0.0.9" }),
      makePair({ id: 2, destination_ip: "10.0.0.1" }),
    ];
    setupMocks(pairs);
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "running" }));

    // Initial default sort is state asc. Click Destination → desc first.
    await user.click(screen.getByRole("button", { name: /destination/i }));
    let rowOrder = screen
      .getAllByTestId(/pair-row-\d/)
      .map((row) => row.getAttribute("data-pair-destination"));
    expect(rowOrder).toEqual(["10.0.0.9", "10.0.0.1"]);

    // Second click flips to asc.
    await user.click(screen.getByRole("button", { name: /destination/i }));
    rowOrder = screen
      .getAllByTestId(/pair-row-\d/)
      .map((row) => row.getAttribute("data-pair-destination"));
    expect(rowOrder).toEqual(["10.0.0.1", "10.0.0.9"]);
  });
});

describe("PairsTab — row actions", () => {
  test("force re-measure pair fires useForcePair with the row's (source, destination)", async () => {
    setupMocks([makePair({ id: 1, source_agent_id: "agent-a", destination_ip: "10.0.0.1" })]);
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "running" }));

    await user.click(screen.getByLabelText(/actions for pair agent-a.*10\.0\.0\.1/i));
    await user.click(screen.getByText(/force re-measure pair/i));

    expect(forcePairStub.mutate).toHaveBeenCalledTimes(1);
    const [vars] = forcePairStub.mutate.mock.calls[0];
    expect(vars).toEqual({
      id: CAMPAIGN_ID,
      body: { source_agent_id: "agent-a", destination_ip: "10.0.0.1" },
    });
  });

  test("dispatch detail for pair fires useTriggerDetail with scope=pair", async () => {
    setupMocks([makePair({ id: 1, source_agent_id: "agent-a", destination_ip: "10.0.0.1" })]);
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    await user.click(screen.getByLabelText(/actions for pair agent-a.*10\.0\.0\.1/i));
    await user.click(screen.getByText(/dispatch detail for this pair/i));

    expect(triggerDetailStub.mutate).toHaveBeenCalledTimes(1);
    const [vars] = triggerDetailStub.mutate.mock.calls[0];
    expect(vars).toEqual({
      id: CAMPAIGN_ID,
      body: {
        scope: "pair",
        pair: { source_agent_id: "agent-a", destination_ip: "10.0.0.1" },
      },
    });
  });

  test("no_pairs_selected on detail dispatch surfaces a dedicated toast", async () => {
    setupMocks([makePair({ id: 1, source_agent_id: "agent-a", destination_ip: "10.0.0.1" })]);
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    const err = new Error("failed", { cause: { error: "no_pairs_selected" } });
    triggerDetailStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByLabelText(/actions for pair agent-a.*10\.0\.0\.1/i));
    await user.click(screen.getByText(/dispatch detail for this pair/i));

    await waitFor(() => {
      expect(screen.getByText(/no pairs qualified/i)).toBeInTheDocument();
    });
  });

  test("illegal_state_transition on force pair surfaces a dedicated toast", async () => {
    setupMocks([makePair({ id: 1, source_agent_id: "agent-a", destination_ip: "10.0.0.1" })]);
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "running" }));

    const err = new Error("failed", { cause: { error: "illegal_state_transition" } });
    forcePairStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByLabelText(/actions for pair agent-a.*10\.0\.0\.1/i));
    await user.click(screen.getByText(/force re-measure pair/i));

    await waitFor(() => {
      expect(screen.getByText(/campaign advanced before the request landed/i)).toBeInTheDocument();
    });
  });
});

// ---------------------------------------------------------------------------
// edge_candidate mode
// ---------------------------------------------------------------------------

describe("PairsTab — edge_candidate mode", () => {
  test("renders the not-evaluated placeholder when evaluation is null", () => {
    // Pre-/evaluate state: GET /evaluation/edge_pairs returns 404
    // `not_evaluated`, which would surface as the EdgePairsTab error
    // alert. Render a placeholder instead and skip the API call.
    setupMocks([]);
    renderTab(
      makeCampaign({ state: "running", evaluation_mode: "edge_candidate" }),
      null,
    );
    expect(screen.getByTestId("edge-pairs-placeholder")).toBeInTheDocument();
    expect(screen.queryByTestId("edge-pairs-tab")).not.toBeInTheDocument();
    expect(useEdgePairDetails).not.toHaveBeenCalled();
  });

  test("renders EdgePairsTab (data-testid=edge-pairs-tab) instead of pairs-tab", () => {
    setupMocks([]);
    renderTab(makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }));
    expect(screen.getByTestId("edge-pairs-tab")).toBeInTheDocument();
    expect(screen.queryByTestId("pairs-tab")).not.toBeInTheDocument();
  });

  test("does not call useCampaignPairs in edge_candidate mode", () => {
    setupMocks([]);
    renderTab(makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }));
    expect(useCampaignPairs).not.toHaveBeenCalled();
  });

  test("calls useEdgePairDetails with campaign id and no candidate_ip filter", () => {
    setupMocks([]);
    renderTab(makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }));
    expect(useEdgePairDetails).toHaveBeenCalledWith(
      CAMPAIGN_ID,
      expect.not.objectContaining({ candidate_ip: expect.anything() }),
    );
  });

  test("renders empty state when edge pairs list is empty", () => {
    setupMocks([]);
    renderTab(makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }));
    expect(screen.getByText(/no edge pair data for this campaign yet/i)).toBeInTheDocument();
  });

  test("renders one row per edge pair with candidate ip and route shape", () => {
    setupMocks([]);
    vi.mocked(useEdgePairDetails).mockReturnValue({
      data: {
        pages: [
          {
            entries: [
              makeEdgePairRow("10.0.55.1", "agent-b", { best_route_kind: "direct", qualifies_under_t: true }),
              makeEdgePairRow("10.0.55.2", "agent-c", { best_route_kind: "one_hop", qualifies_under_t: false }),
            ],
            next_cursor: null,
          },
        ],
        pageParams: [null],
      },
      isLoading: false,
      isError: false,
      error: null,
      hasNextPage: false,
      isFetchingNextPage: false,
      fetchNextPage: vi.fn(),
      refetch: vi.fn(),
    } as unknown as ReturnType<typeof useEdgePairDetails>);

    renderTab(makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }));

    expect(screen.getByTestId("edge-pair-row-0")).toBeInTheDocument();
    expect(screen.getByTestId("edge-pair-row-1")).toBeInTheDocument();
    // Route kind chips
    expect(screen.getByText("direct")).toBeInTheDocument();
    expect(screen.getByText("1 hop")).toBeInTheDocument();
  });

  test("shows 'qualifies' badge for qualifying rows and 'above T' for non-qualifying", () => {
    setupMocks([]);
    vi.mocked(useEdgePairDetails).mockReturnValue({
      data: {
        pages: [
          {
            entries: [
              makeEdgePairRow("10.0.55.1", "agent-b", { qualifies_under_t: true }),
              makeEdgePairRow("10.0.55.2", "agent-c", { qualifies_under_t: false }),
            ],
            next_cursor: null,
          },
        ],
        pageParams: [null],
      },
      isLoading: false,
      isError: false,
      error: null,
      hasNextPage: false,
      isFetchingNextPage: false,
      fetchNextPage: vi.fn(),
      refetch: vi.fn(),
    } as unknown as ReturnType<typeof useEdgePairDetails>);

    renderTab(makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }));

    expect(screen.getByText("qualifies")).toBeInTheDocument();
    expect(screen.getByText("above T")).toBeInTheDocument();
  });

  test("renders skeleton while edge pairs are loading", () => {
    setupMocks([]);
    vi.mocked(useEdgePairDetails).mockReturnValue({
      data: undefined,
      isLoading: true,
      isError: false,
      error: null,
      hasNextPage: false,
      isFetchingNextPage: false,
      fetchNextPage: vi.fn(),
      refetch: vi.fn(),
    } as unknown as ReturnType<typeof useEdgePairDetails>);

    renderTab(makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }));
    expect(screen.getByTestId("edge-pairs-tab")).toHaveAttribute("role", "status");
  });

  test("renders error card when edge pairs fetch fails", () => {
    setupMocks([]);
    vi.mocked(useEdgePairDetails).mockReturnValue({
      data: undefined,
      isLoading: false,
      isError: true,
      error: new Error("network failure"),
      hasNextPage: false,
      isFetchingNextPage: false,
      fetchNextPage: vi.fn(),
      refetch: vi.fn(),
    } as unknown as ReturnType<typeof useEdgePairDetails>);

    renderTab(makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/failed to load edge pairs/i);
  });
});
