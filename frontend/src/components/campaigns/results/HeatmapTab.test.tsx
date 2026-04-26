/**
 * Tests for HeatmapTab — covers O1 (basic render), O2 (color tiers),
 * O3 (sort + cell click drilldown), and O4 (virtualization).
 */
import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { Campaign, CampaignState } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";
import type { EvaluationEdgePairDetailDto } from "@/api/hooks/evaluation";
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
// Module mocks
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
import { getTier, readBoundaries, HeatmapTab } from "@/components/campaigns/results/HeatmapTab";

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
          pairs_total_considered: 2,
          avg_improvement_ms: null,
          avg_loss_ratio: 0,
          composite_score: null,
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
          pairs_total_considered: 2,
          avg_improvement_ms: null,
          avg_loss_ratio: 0,
          composite_score: null,
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
  is_unreachable = false,
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
    qualifies_under_t: best_route_ms < 100,
    is_unreachable,
  };
}

function makeQueryResult(
  rows: EvaluationEdgePairDetailDto[],
  opts?: { isLoading?: boolean; isError?: boolean; hasNextPage?: boolean },
) {
  return {
    data: { pages: [{ entries: rows, next_cursor: null, total: rows.length }], pageParams: [null] },
    isLoading: opts?.isLoading ?? false,
    isError: opts?.isError ?? false,
    error: null,
    isFetchingNextPage: false,
    hasNextPage: opts?.hasNextPage ?? false,
    fetchNextPage: vi.fn(),
    refetch: vi.fn(),
  };
}

function makeQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
}

function renderHeatmap(
  rows: EvaluationEdgePairDetailDto[],
  evaluation?: Partial<Evaluation>,
  opts?: { isLoading?: boolean; isError?: boolean },
) {
  vi.mocked(useEdgePairDetails).mockReturnValue(
    makeQueryResult(rows, opts) as unknown as ReturnType<typeof useEdgePairDetails>,
  );

  const qc = makeQueryClient();
  const campaign = makeCampaign({ state: "evaluated" });
  const ev = makeEvaluation(evaluation);

  return render(
    <QueryClientProvider client={qc}>
      <IpHostnameProvider>
        <HeatmapTab campaign={campaign} evaluation={ev} />
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
}

// ---------------------------------------------------------------------------
// Setup/teardown
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
// O1: Basic render
// ---------------------------------------------------------------------------

describe("O1: HeatmapTab basic render", () => {
  test("renders column headers for each candidate IP", () => {
    const rows = [
      makeEdgePairRow("10.0.0.1", "agent-a", 30),
      makeEdgePairRow("10.0.0.2", "agent-a", 50),
      makeEdgePairRow("10.0.0.1", "agent-b", 120),
      makeEdgePairRow("10.0.0.2", "agent-b", 200),
    ];
    renderHeatmap(rows);

    expect(screen.getByTestId("heatmap-col-header-10.0.0.1")).toBeInTheDocument();
    expect(screen.getByTestId("heatmap-col-header-10.0.0.2")).toBeInTheDocument();
  });

  test("renders a row for each destination agent", () => {
    const rows = [
      makeEdgePairRow("10.0.0.1", "agent-a", 30),
      makeEdgePairRow("10.0.0.1", "agent-b", 50),
    ];
    renderHeatmap(rows);

    expect(screen.getByTestId("heatmap-row-agent-a")).toBeInTheDocument();
    expect(screen.getByTestId("heatmap-row-agent-b")).toBeInTheDocument();
  });

  test("cell shows integer ms value", () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 42.7)];
    renderHeatmap(rows);

    const cell = screen.getByTestId("heatmap-cell-10.0.0.1-agent-a");
    expect(cell).toHaveTextContent("43");
  });

  test("shows loading skeleton while fetching", () => {
    renderHeatmap([], {}, { isLoading: true });

    const section = screen.getByTestId("heatmap-tab");
    expect(section).toHaveAttribute("role", "status");
  });

  test("shows error card on fetch failure", () => {
    vi.mocked(useEdgePairDetails).mockReturnValue({
      data: undefined,
      isLoading: false,
      isError: true,
      error: new Error("fetch failed"),
      isFetchingNextPage: false,
      hasNextPage: false,
      fetchNextPage: vi.fn(),
      refetch: vi.fn(),
    } as unknown as ReturnType<typeof useEdgePairDetails>);

    const qc = makeQueryClient();
    render(
      <QueryClientProvider client={qc}>
        <IpHostnameProvider>
          <HeatmapTab campaign={makeCampaign({ state: "evaluated" })} evaluation={makeEvaluation()} />
        </IpHostnameProvider>
      </QueryClientProvider>,
    );

    expect(screen.getByRole("alert")).toBeInTheDocument();
  });

  test("shows empty state when no rows", () => {
    renderHeatmap([]);

    expect(screen.getByRole("status")).toBeInTheDocument();
    expect(screen.getByText(/no edge pair data/i)).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// O2: Color tier rendering
// ---------------------------------------------------------------------------

describe("O2: Color tier rendering", () => {
  describe("getTier", () => {
    test("tier 1 below 0.4·T", () => {
      // T=100, boundaries=[40,100,200,400]
      expect(getTier(10, [40, 100, 200, 400])).toBe(1);
      expect(getTier(39.9, [40, 100, 200, 400])).toBe(1);
    });

    test("tier 2 between 0.4·T and T", () => {
      expect(getTier(40, [40, 100, 200, 400])).toBe(2);
      expect(getTier(99.9, [40, 100, 200, 400])).toBe(2);
    });

    test("tier 3 between T and 2·T", () => {
      expect(getTier(100, [40, 100, 200, 400])).toBe(3);
      expect(getTier(199.9, [40, 100, 200, 400])).toBe(3);
    });

    test("tier 4 between 2·T and 4·T", () => {
      expect(getTier(200, [40, 100, 200, 400])).toBe(4);
      expect(getTier(399.9, [40, 100, 200, 400])).toBe(4);
    });

    test("tier 5 at or above 4·T", () => {
      expect(getTier(400, [40, 100, 200, 400])).toBe(5);
      expect(getTier(9999, [40, 100, 200, 400])).toBe(5);
    });
  });

  test("unreachable cell shows dash text", () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 0, true)];
    renderHeatmap(rows);

    const cell = screen.getByTestId("heatmap-cell-10.0.0.1-agent-a");
    expect(cell).toHaveTextContent("—");
  });

  test("boundaries default from useful_latency_ms", () => {
    // T=80, boundaries=[32,80,160,320]
    const boundaries = readBoundaries("edge_candidate", 80);
    expect(boundaries).toEqual([32, 80, 160, 320]);
  });

  test("boundaries default to T=80 when useful_latency_ms is null", () => {
    const boundaries = readBoundaries("edge_candidate", null);
    expect(boundaries).toEqual([32, 80, 160, 320]);
  });

  test("boundaries read from localStorage when present", () => {
    localStorage.setItem(
      "meshmon.evaluation.heatmap.edge_candidate.colors",
      JSON.stringify([10, 20, 30, 40]),
    );
    const boundaries = readBoundaries("edge_candidate", 80);
    expect(boundaries).toEqual([10, 20, 30, 40]);
  });

  test("boundaries fallback to default when localStorage value is invalid", () => {
    localStorage.setItem("meshmon.evaluation.heatmap.edge_candidate.colors", "not-json{{{");
    const boundaries = readBoundaries("edge_candidate", 80);
    expect(boundaries).toEqual([32, 80, 160, 320]);
  });
});

// ---------------------------------------------------------------------------
// O3: Sortable rows/columns + cell click
// ---------------------------------------------------------------------------

describe("O3: Sort + cell click", () => {
  test("row sort buttons are rendered", () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 30)];
    renderHeatmap(rows);

    // Row sort options
    expect(screen.getByRole("button", { name: /agent id/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /mean rtt/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /qualifying/i })).toBeInTheDocument();
  });

  test("col sort buttons are rendered", () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 30)];
    renderHeatmap(rows);

    expect(screen.getByRole("button", { name: /candidate ip/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /weighted rtt/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /coverage/i })).toBeInTheDocument();
  });

  test("cell click opens drilldown dialog via DrilldownDialog", async () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 42)];
    renderHeatmap(rows);

    const cell = screen.getByTestId("heatmap-cell-10.0.0.1-agent-a");
    fireEvent.click(cell);

    // DrilldownDialog only renders body when candidate !== null;
    // with mock evaluation the candidate is found and dialog opens.
    await waitFor(() => {
      // The dialog will be open (aria-expanded or role=dialog)
      expect(screen.getByRole("dialog")).toBeInTheDocument();
    });
  });

  test("cell aria-label conveys ms value", () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 75)];
    renderHeatmap(rows);

    const cell = screen.getByTestId("heatmap-cell-10.0.0.1-agent-a");
    expect(cell).toHaveAttribute(
      "aria-label",
      expect.stringContaining("75 ms"),
    );
  });

  test("unreachable cell aria-label says unreachable", () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 0, true)];
    renderHeatmap(rows);

    const cell = screen.getByTestId("heatmap-cell-10.0.0.1-agent-a");
    expect(cell).toHaveAttribute("aria-label", expect.stringContaining("unreachable"));
  });
});

// ---------------------------------------------------------------------------
// O4: Virtualization
// ---------------------------------------------------------------------------

describe("O4: Virtualization", () => {
  test("renders all rows when under threshold (≤30)", () => {
    const rows: EvaluationEdgePairDetailDto[] = [];
    for (let a = 0; a < 5; a++) {
      for (let c = 0; c < 5; c++) {
        rows.push(makeEdgePairRow(`10.0.${c}.1`, `agent-${a}`, 30 + a + c));
      }
    }
    renderHeatmap(rows);

    // All 5 agents should be in DOM
    for (let a = 0; a < 5; a++) {
      expect(screen.getByTestId(`heatmap-row-agent-${a}`)).toBeInTheDocument();
    }
  });

  test("rows wrapper has explicit height when virtualization is disabled", () => {
    // Each absolute-positioned row needs its parent to declare a non-zero
    // height; otherwise the wrapper collapses and rows clip out of flow.
    const rows: EvaluationEdgePairDetailDto[] = [];
    for (let a = 0; a < 3; a++) {
      rows.push(makeEdgePairRow("10.0.0.1", `agent-${a}`, 30 + a));
    }
    renderHeatmap(rows);

    const firstRow = screen.getByTestId("heatmap-row-agent-0");
    const wrapper = firstRow.parentElement as HTMLElement;
    expect(wrapper).not.toBeNull();
    // Three rows × ROW_HEIGHT (40px each) = 120px.
    expect(wrapper.style.height).toBe("120px");
  });

  test("with 50 candidates renders only a window of col headers (virtualized)", () => {
    const agents = ["agent-a", "agent-b"];
    const candidates = Array.from({ length: 50 }, (_, i) => `10.0.${i}.1`);
    const rows: EvaluationEdgePairDetailDto[] = [];
    for (const a of agents) {
      for (const c of candidates) {
        rows.push(makeEdgePairRow(c, a, 30));
      }
    }
    renderHeatmap(rows);

    // There are 50 candidates total; with virtualizer only a subset of
    // col headers should be in the DOM. The overscan=5 + visible window
    // means we expect significantly fewer than 50 headers.
    const colHeaders = screen
      .getAllByTestId(/^heatmap-col-header-/)
      .filter((el) => el.getAttribute("data-testid")?.startsWith("heatmap-col-header-10.0."));
    expect(colHeaders.length).toBeGreaterThan(0);
    expect(colHeaders.length).toBeLessThan(50);
  });

  test("with 50 agents renders only a window of rows (virtualized)", () => {
    const agents = Array.from({ length: 50 }, (_, i) => `agent-${i}`);
    const candidates = ["10.0.0.1"];
    const rows: EvaluationEdgePairDetailDto[] = [];
    for (const a of agents) {
      for (const c of candidates) {
        rows.push(makeEdgePairRow(c, a, 30));
      }
    }
    renderHeatmap(rows);

    // With 50 agents, virtualization kicks in. Only a subset should render.
    const agentRows = screen.getAllByTestId(/^heatmap-row-/);
    expect(agentRows.length).toBeGreaterThan(0);
    expect(agentRows.length).toBeLessThan(50);
  });
});
