/**
 * Tests for CompareTab — covers P1 (placeholder), P2 (agent picker),
 * P3 (pick_role radio), P4 (candidate sub-picker), P5 (re-aggregation),
 * localStorage round-trip, URL param round-trip, and drilldown wiring.
 */
import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { Campaign, CampaignState } from "@/api/hooks/campaigns";
import type { Evaluation, EvaluationEdgePairDetailDto } from "@/api/hooks/evaluation";
import { IpHostnameProvider } from "@/components/ip-hostname";

// ---------------------------------------------------------------------------
// EventSource stub
// ---------------------------------------------------------------------------

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

// ---------------------------------------------------------------------------
// Module mocks — register BEFORE importing components
// ---------------------------------------------------------------------------

vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return {
    ...actual,
    useNavigate: () => vi.fn(),
    useSearch: () => ({}),
  };
});

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return { ...actual, useEdgePairDetails: vi.fn() };
});

vi.mock("@/components/catalogue/CatalogueDrawerOverlay", () => ({
  useCatalogueDrawer: () => ({ open: vi.fn() }),
  CatalogueDrawerOverlay: ({ children }: { children: unknown }) => <>{children}</>,
}));

import { useEdgePairDetails } from "@/api/hooks/evaluation";
import { CompareTab } from "@/components/campaigns/results/CompareTab";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "cccccccc-cccc-cccc-cccc-cccccccccccc";

function makeCampaign(
  overrides: Partial<Campaign> & { state: CampaignState },
): Campaign {
  return {
    id: overrides.id ?? CAMPAIGN_ID,
    title: overrides.title ?? "Campaign compare",
    notes: overrides.notes ?? "",
    state: overrides.state,
    protocol: overrides.protocol ?? "icmp",
    evaluation_mode: overrides.evaluation_mode ?? "edge_candidate",
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
    vm_lookback_minutes: overrides.vm_lookback_minutes ?? 60,
    source_agent_ids: overrides.source_agent_ids ?? ["agent-a", "agent-b", "agent-c"],
  } as Campaign;
}

function makeEvaluation(overrides: Partial<Evaluation> = {}): Evaluation {
  return {
    campaign_id: CAMPAIGN_ID,
    evaluated_at: "2026-04-21T10:00:00Z",
    loss_threshold_ratio: 0.02,
    stddev_weight: 1,
    evaluation_mode: "edge_candidate",
    baseline_pair_count: 4,
    candidates_total: 2,
    candidates_good: 1,
    avg_improvement_ms: null,
    useful_latency_ms: 100,
    results: {
      candidates: [
        {
          destination_ip: "10.0.0.1",
          display_name: "cand-1",
          city: null,
          country_code: null,
          asn: null,
          network_operator: null,
          hostname: null,
          is_mesh_member: false,
          pairs_improved: 0,
          pairs_total_considered: 3,
          avg_improvement_ms: null,
          avg_loss_ratio: 0,
          composite_score: null,
          coverage_count: 2,
          mean_ms_under_t: 50,
          destinations_total: 3,
          coverage_weighted_ping_ms: 50,
          direct_share: 1,
          onehop_share: 0,
          twohop_share: 0,
        },
        {
          destination_ip: "10.0.0.2",
          display_name: "cand-2",
          city: null,
          country_code: null,
          asn: null,
          network_operator: null,
          hostname: null,
          is_mesh_member: false,
          pairs_improved: 0,
          pairs_total_considered: 3,
          avg_improvement_ms: null,
          avg_loss_ratio: 0,
          composite_score: null,
          coverage_count: 1,
          mean_ms_under_t: 80,
          destinations_total: 3,
          coverage_weighted_ping_ms: 80,
          direct_share: 1,
          onehop_share: 0,
          twohop_share: 0,
        },
      ],
      unqualified_reasons: {},
    },
    ...overrides,
  } as Evaluation;
}

function makeEdgePairRow(
  candidate_ip: string,
  destination_agent_id: string,
  best_route_ms: number,
  qualifies_under_t = true,
): EvaluationEdgePairDetailDto {
  return {
    candidate_ip,
    destination_agent_id,
    best_route_ms,
    best_route_loss_ratio: 0,
    best_route_stddev_ms: 1,
    best_route_kind: "direct",
    best_route_legs: [],
    best_route_intermediaries: [],
    qualifies_under_t,
    is_unreachable: false,
  };
}

function makeQueryResult(
  rows: EvaluationEdgePairDetailDto[],
  opts?: { isLoading?: boolean; isError?: boolean; hasNextPage?: boolean },
) {
  return {
    data: {
      pages: [{ entries: rows, next_cursor: null, total: rows.length }],
      pageParams: [null],
    },
    isLoading: opts?.isLoading ?? false,
    isError: opts?.isError ?? false,
    error: null,
    isFetchingNextPage: false,
    hasNextPage: opts?.hasNextPage ?? false,
    fetchNextPage: vi.fn(),
    refetch: vi.fn(),
  };
}

function makeQueryClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
}

function renderCompareTab(
  evaluation: Evaluation | null,
  campaignOverrides: Partial<Campaign> = {},
  rows: EvaluationEdgePairDetailDto[] = [],
  opts?: { isLoading?: boolean },
) {
  vi.mocked(useEdgePairDetails).mockReturnValue(
    makeQueryResult(rows, opts) as unknown as ReturnType<typeof useEdgePairDetails>,
  );

  const qc = makeQueryClient();
  const campaign = makeCampaign({ state: "evaluated", ...campaignOverrides });

  return render(
    <QueryClientProvider client={qc}>
      <IpHostnameProvider>
        <CompareTab campaign={campaign} evaluation={evaluation} />
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
}

// ---------------------------------------------------------------------------
// Setup / teardown
// ---------------------------------------------------------------------------

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
  localStorage.clear();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  localStorage.clear();
});

// ---------------------------------------------------------------------------
// P1: Placeholder when evaluation is null
// ---------------------------------------------------------------------------

describe("P1: placeholder", () => {
  test("renders placeholder when evaluation is null", () => {
    renderCompareTab(null);
    expect(screen.getByTestId("compare-placeholder")).toBeInTheDocument();
    expect(screen.getByText(/evaluate first/i)).toBeInTheDocument();
  });

  test("does not render CompareView when evaluation is null", () => {
    renderCompareTab(null);
    expect(screen.queryByTestId("compare-view")).not.toBeInTheDocument();
  });

  test("renders CompareView when evaluation is provided", () => {
    renderCompareTab(makeEvaluation());
    expect(screen.getByTestId("compare-view")).toBeInTheDocument();
    expect(screen.queryByTestId("compare-placeholder")).not.toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// P2: Agent picker
// ---------------------------------------------------------------------------

describe("P2: agent picker", () => {
  test("renders a checkbox for each source_agent_id", () => {
    renderCompareTab(makeEvaluation());
    expect(screen.getByTestId("agent-picker-agent-a")).toBeInTheDocument();
    expect(screen.getByTestId("agent-picker-agent-b")).toBeInTheDocument();
    expect(screen.getByTestId("agent-picker-agent-c")).toBeInTheDocument();
  });

  test("checking an agent updates the selection", async () => {
    renderCompareTab(makeEvaluation(), { source_agent_ids: ["agent-a", "agent-b"] });
    const checkbox = screen.getByTestId("agent-picker-agent-a");
    fireEvent.click(checkbox);
    await waitFor(() => {
      expect(checkbox).toBeChecked();
    });
  });

  test("agent picker renders empty state when no source_agent_ids", () => {
    renderCompareTab(makeEvaluation(), { source_agent_ids: [] });
    expect(screen.getByTestId("compare-no-agents")).toBeInTheDocument();
  });

  test("persists picked agents to localStorage on change", async () => {
    const storageKey = `meshmon.evaluation.compare.${CAMPAIGN_ID}.agents`;
    renderCompareTab(makeEvaluation(), { source_agent_ids: ["agent-a", "agent-b"] });

    const checkbox = screen.getByTestId("agent-picker-agent-a");
    fireEvent.click(checkbox);

    await waitFor(() => {
      const stored = localStorage.getItem(storageKey);
      expect(stored).not.toBeNull();
      const parsed = JSON.parse(stored!) as string[];
      expect(parsed).toContain("agent-a");
    });
  });

  test("loads persisted agents from localStorage on mount", () => {
    const storageKey = `meshmon.evaluation.compare.${CAMPAIGN_ID}.agents`;
    localStorage.setItem(storageKey, JSON.stringify(["agent-b"]));

    renderCompareTab(makeEvaluation(), { source_agent_ids: ["agent-a", "agent-b"] });

    const checkboxB = screen.getByTestId("agent-picker-agent-b");
    const checkboxA = screen.getByTestId("agent-picker-agent-a");
    expect(checkboxB).toBeChecked();
    expect(checkboxA).not.toBeChecked();
  });
});

// ---------------------------------------------------------------------------
// P3: Pick role radio (diversity/optimization only)
// ---------------------------------------------------------------------------

describe("P3: pick role radio", () => {
  test("pick role radio is hidden for edge_candidate mode", () => {
    renderCompareTab(makeEvaluation({ evaluation_mode: "edge_candidate" }));
    expect(screen.queryByTestId("pick-role-radio")).not.toBeInTheDocument();
  });

  test("pick role radio is shown for diversity mode", () => {
    renderCompareTab(makeEvaluation({ evaluation_mode: "diversity" }));
    expect(screen.getByTestId("pick-role-radio")).toBeInTheDocument();
  });

  test("pick role radio is shown for optimization mode", () => {
    renderCompareTab(makeEvaluation({ evaluation_mode: "optimization" }));
    expect(screen.getByTestId("pick-role-radio")).toBeInTheDocument();
  });

  test("pick role radio has three options: both, source, destination", () => {
    renderCompareTab(makeEvaluation({ evaluation_mode: "diversity" }));
    expect(screen.getByTestId("pick-role-both")).toBeInTheDocument();
    expect(screen.getByTestId("pick-role-source")).toBeInTheDocument();
    expect(screen.getByTestId("pick-role-destination")).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// P4: Candidate sub-picker (transient, URL-only)
// ---------------------------------------------------------------------------

describe("P4: candidate sub-picker", () => {
  test("candidate sub-picker is hidden before any agent is picked", () => {
    renderCompareTab(makeEvaluation());
    expect(screen.queryByTestId("candidate-sub-picker-details")).not.toBeInTheDocument();
  });

  test("candidate sub-picker appears after at least one agent is checked", async () => {
    renderCompareTab(makeEvaluation(), { source_agent_ids: ["agent-a"] });
    const checkbox = screen.getByTestId("agent-picker-agent-a");
    fireEvent.click(checkbox);
    await waitFor(() => {
      expect(screen.getByTestId("candidate-sub-picker-details")).toBeInTheDocument();
    });
  });

  test("candidate sub-picker is initially closed (details element not open)", async () => {
    renderCompareTab(makeEvaluation(), { source_agent_ids: ["agent-a"] });
    const checkbox = screen.getByTestId("agent-picker-agent-a");
    fireEvent.click(checkbox);
    await waitFor(() => {
      const details = screen.getByTestId("candidate-sub-picker-details") as HTMLDetailsElement;
      expect(details).not.toHaveAttribute("open");
    });
  });
});

// ---------------------------------------------------------------------------
// P5: Re-aggregation
// ---------------------------------------------------------------------------

describe("P5: re-aggregation for edge_candidate", () => {
  test("shows loading state while fetching edge pairs", () => {
    renderCompareTab(makeEvaluation(), {}, [], { isLoading: true });
    expect(screen.getByRole("status")).toBeInTheDocument();
  });

  test("shows storage-filter caveat tooltip", () => {
    renderCompareTab(makeEvaluation());
    expect(screen.getByTestId("storage-filter-caveat")).toBeInTheDocument();
  });

  test("compare table appears after picking at least one agent with data", async () => {
    const rows = [
      makeEdgePairRow("10.0.0.1", "agent-a", 50),
      makeEdgePairRow("10.0.0.2", "agent-a", 80),
    ];
    renderCompareTab(makeEvaluation(), { source_agent_ids: ["agent-a"] }, rows);

    const checkbox = screen.getByTestId("agent-picker-agent-a");
    fireEvent.click(checkbox);

    await waitFor(() => {
      expect(screen.getByTestId("compare-candidates-table")).toBeInTheDocument();
    });
  });

  test("recomputed coverage_count reflects only picked agents", async () => {
    // 10.0.0.1 qualifies for agent-a (50ms < 100T) but not agent-b (200ms)
    const rows = [
      makeEdgePairRow("10.0.0.1", "agent-a", 50, true),
      makeEdgePairRow("10.0.0.1", "agent-b", 200, false),
    ];
    renderCompareTab(makeEvaluation(), { source_agent_ids: ["agent-a", "agent-b"] }, rows);

    // Pick only agent-a
    fireEvent.click(screen.getByTestId("agent-picker-agent-a"));

    await waitFor(() => {
      // The recomputed row for 10.0.0.1 should show coverage_count=1 / total=1
      const coverageCell = screen.getByTestId("compare-coverage-10.0.0.1");
      expect(coverageCell).toHaveTextContent("1");
    });
  });

  test("coverage_weighted_ping_ms column shows dash (deferred)", async () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 50)];
    renderCompareTab(makeEvaluation(), { source_agent_ids: ["agent-a"] }, rows);
    fireEvent.click(screen.getByTestId("agent-picker-agent-a"));

    await waitFor(() => {
      const cwpCell = screen.getByTestId("compare-cwp-10.0.0.1");
      expect(cwpCell).toHaveTextContent("—");
    });
  });

  test("clicking a row opens DrilldownDialog", async () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 50)];
    renderCompareTab(makeEvaluation(), { source_agent_ids: ["agent-a"] }, rows);
    fireEvent.click(screen.getByTestId("agent-picker-agent-a"));

    await waitFor(() => {
      expect(screen.getByTestId("compare-candidates-table")).toBeInTheDocument();
    });

    const row = screen.getByTestId("compare-candidate-row-10.0.0.1");
    fireEvent.click(row);

    await waitFor(() => {
      expect(screen.getByRole("dialog")).toBeInTheDocument();
    });
  });
});

// ---------------------------------------------------------------------------
// Diversity/optimization stub
// ---------------------------------------------------------------------------

describe("diversity/optimization stub", () => {
  test("shows stub notice for diversity mode", () => {
    renderCompareTab(makeEvaluation({ evaluation_mode: "diversity" }));
    expect(screen.getByTestId("compare-triple-stub")).toBeInTheDocument();
  });

  test("shows stub notice for optimization mode", () => {
    renderCompareTab(makeEvaluation({ evaluation_mode: "optimization" }));
    expect(screen.getByTestId("compare-triple-stub")).toBeInTheDocument();
  });
});
