import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { Campaign } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";
import type {
  EvaluationPairDetailListResponse,
  PairDetailsQuery,
} from "@/api/hooks/evaluation-pairs";
import { IpHostnameProvider } from "@/components/ip-hostname";

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
  return { ...actual, useCampaignMeasurements: vi.fn() };
});

vi.mock("@/api/hooks/evaluation-pairs", async () => {
  const actual = await vi.importActual<typeof import("@/api/hooks/evaluation-pairs")>(
    "@/api/hooks/evaluation-pairs",
  );
  return { ...actual, useCandidatePairDetails: vi.fn() };
});

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return { ...actual, useEdgePairDetails: vi.fn() };
});

vi.mock("@/components/RouteTopology", () => ({
  RouteTopology: () => <div data-testid="route-topology" />,
}));

vi.mock("@/components/catalogue/CatalogueDrawerOverlay", () => ({
  useCatalogueDrawer: () => ({ open: vi.fn() }),
  CatalogueDrawerOverlay: ({ children }: { children: ReactNode }) => <>{children}</>,
}));

vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return {
    ...actual,
    useNavigate: () => vi.fn(),
  };
});

import { useAgents } from "@/api/hooks/agents";
import { useCampaignMeasurements } from "@/api/hooks/campaigns";
import { useEdgePairDetails } from "@/api/hooks/evaluation";
import { useCandidatePairDetails } from "@/api/hooks/evaluation-pairs";
import { DrilldownDialog } from "@/components/campaigns/results/DrilldownDialog";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "11111111-1111-1111-1111-111111111111";
const CANDIDATE_IP = "10.0.99.1";

function makeCampaign(): Campaign {
  return {
    id: CAMPAIGN_ID,
    title: "t",
    notes: "",
    state: "evaluated",
    protocol: "icmp",
    evaluation_mode: "optimization",
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
    pair_counts: [["succeeded", 6]],
  } as unknown as Campaign;
}

function makeCandidate(
  overrides: Partial<Evaluation["results"]["candidates"][number]> = {},
): Evaluation["results"]["candidates"][number] {
  return {
    destination_ip: CANDIDATE_IP,
    display_name: overrides.display_name ?? "transit-x",
    city: null,
    country_code: null,
    asn: null,
    network_operator: null,
    is_mesh_member: overrides.is_mesh_member ?? false,
    pairs_improved: overrides.pairs_improved ?? 5,
    pairs_total_considered: overrides.pairs_total_considered ?? 100,
    avg_improvement_ms: 22,
    avg_loss_ratio: 0.0005,
    composite_score: 10,
    hostname: null,
  };
}

function makeEvaluation(overrides: Partial<Evaluation> = {}): Evaluation {
  return {
    campaign_id: CAMPAIGN_ID,
    evaluated_at: "2026-04-21T10:00:00Z",
    loss_threshold_ratio: 0.02,
    stddev_weight: 1,
    evaluation_mode: "optimization",
    baseline_pair_count: 100,
    candidates_total: 1,
    candidates_good: 1,
    avg_improvement_ms: 20,
    max_transit_rtt_ms: overrides.max_transit_rtt_ms ?? null,
    max_transit_stddev_ms: overrides.max_transit_stddev_ms ?? null,
    min_improvement_ms: overrides.min_improvement_ms ?? null,
    min_improvement_ratio: overrides.min_improvement_ratio ?? null,
    results: { candidates: [], unqualified_reasons: {} },
    ...overrides,
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

function makeEntry(idx: number, overrides: Record<string, unknown> = {}) {
  return {
    source_agent_id: `agent-${idx}-src`,
    destination_agent_id: `agent-${idx}-dst`,
    destination_ip: CANDIDATE_IP,
    direct_rtt_ms: 50,
    direct_stddev_ms: 1,
    direct_loss_ratio: 0.001,
    direct_source: "active_probe" as const,
    transit_rtt_ms: 30,
    transit_stddev_ms: 0.5,
    transit_loss_ratio: 0.0005,
    improvement_ms: 20,
    qualifies: true,
    ...overrides,
  };
}

function pageOf(
  entries: ReturnType<typeof makeEntry>[],
  total: number,
): EvaluationPairDetailListResponse {
  return { entries, next_cursor: null, total };
}

interface PairsHookReturn {
  data?: { pages: EvaluationPairDetailListResponse[]; pageParams: Array<string | null> };
  isLoading: boolean;
  isError: boolean;
  isFetchingNextPage: boolean;
  hasNextPage: boolean;
  error: Error | null;
  fetchNextPage: ReturnType<typeof vi.fn>;
  refetch: ReturnType<typeof vi.fn>;
}

function pairsReturn(overrides: Partial<PairsHookReturn> = {}): PairsHookReturn {
  return {
    data: overrides.data ?? {
      pages: [pageOf([], 0)],
      pageParams: [null],
    },
    isLoading: overrides.isLoading ?? false,
    isError: overrides.isError ?? false,
    isFetchingNextPage: overrides.isFetchingNextPage ?? false,
    hasNextPage: overrides.hasNextPage ?? false,
    error: overrides.error ?? null,
    fetchNextPage: overrides.fetchNextPage ?? vi.fn(),
    refetch: overrides.refetch ?? vi.fn(),
  };
}

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

function renderDialog(
  props: Partial<React.ComponentProps<typeof DrilldownDialog>> = {},
  filteredHook: PairsHookReturn = pairsReturn(),
  unfilteredHook: PairsHookReturn = pairsReturn(),
) {
  // The dialog calls `useCandidatePairDetails` twice — first with the
  // active toolbar query, then with the unfiltered (`limit=0`) query.
  // The fake distinguishes the two by inspecting the `limit` argument.
  vi.mocked(useCandidatePairDetails).mockImplementation((_id, _ip, q: PairDetailsQuery) => {
    const r = q.limit === 0 ? unfilteredHook : filteredHook;
    return r as unknown as ReturnType<typeof useCandidatePairDetails>;
  });

  vi.mocked(useAgents).mockReturnValue({
    data: [
      makeAgent("agent-1-src", "alpha", "10.0.0.1"),
      makeAgent("agent-1-dst", "beta", "10.0.0.2"),
    ],
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useAgents>);

  vi.mocked(useCampaignMeasurements).mockReturnValue({
    data: { pages: [{ entries: [], next_cursor: null }], pageParams: [null] },
    isLoading: false,
    isError: false,
    error: null,
  } as unknown as ReturnType<typeof useCampaignMeasurements>);

  // useEdgePairDetails is only called in edge_candidate mode; provide a
  // sensible default so the mock doesn't fail when the dialog renders the
  // TripleDrawerBody branch (which never calls this hook).
  vi.mocked(useEdgePairDetails).mockReturnValue({
    data: { pages: [{ entries: [], next_cursor: null, total: 0 }], pageParams: [null] },
    isLoading: false,
    isError: false,
    isFetchingNextPage: false,
    hasNextPage: false,
    error: null,
    fetchNextPage: vi.fn(),
    refetch: vi.fn(),
  } as unknown as ReturnType<typeof useEdgePairDetails>);

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
  // Resolve candidate via `'candidate' in props`: callers can pass
  // `candidate: null` explicitly to test the closed-state branch.
  const candidate = "candidate" in props ? (props.candidate ?? null) : makeCandidate();
  return render(
    <DrilldownDialog
      candidate={candidate}
      campaign={props.campaign ?? makeCampaign()}
      evaluation={props.evaluation ?? null}
      onClose={props.onClose ?? vi.fn()}
      unqualifiedReason={props.unqualifiedReason}
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

describe("DrilldownDialog — open / close", () => {
  test("mounts when candidate is non-null", () => {
    renderDialog();
    expect(screen.getByTestId("drilldown-body")).toBeInTheDocument();
  });

  test("does not mount the body when candidate is null", () => {
    renderDialog({ candidate: null });
    expect(screen.queryByTestId("drilldown-body")).not.toBeInTheDocument();
  });

  test("close on Escape calls onClose", async () => {
    const onClose = vi.fn();
    const user = userEvent.setup();
    renderDialog({ onClose });
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalled();
  });
});

describe("DrilldownDialog — caption math", () => {
  test('"Showing X of Y rows · Z hidden by storage guardrails" with active guardrails', () => {
    const evaluation = makeEvaluation({ min_improvement_ms: 5 });
    const candidate = makeCandidate({ pairs_total_considered: 100 });
    renderDialog(
      { candidate, evaluation },
      pairsReturn({ data: { pages: [pageOf([makeEntry(1)], 30)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 80)], pageParams: [null] } }),
    );
    const caption = screen.getByTestId("drilldown-caption");
    expect(caption).toHaveTextContent("Showing 30 of 80 rows for this candidate");
    // pairs_total_considered=100, unfiltered.total=80 ⇒ 20 hidden
    expect(caption).toHaveTextContent("20 hidden by storage guardrails");
  });

  test("caption omits the storage-guardrails clause when no guardrails are active", () => {
    const evaluation = makeEvaluation();
    const candidate = makeCandidate({ pairs_total_considered: 100 });
    renderDialog(
      { candidate, evaluation },
      pairsReturn({ data: { pages: [pageOf([makeEntry(1)], 100)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 100)], pageParams: [null] } }),
    );
    const caption = screen.getByTestId("drilldown-caption");
    expect(caption).toHaveTextContent("Showing 100 of 100 rows for this candidate");
    expect(caption).not.toHaveTextContent(/storage guardrails/i);
  });
});

describe("DrilldownDialog — empty / error states", () => {
  test("initial load shows the loading card, not an empty-state card", async () => {
    // First render lands with `isLoading=true` and `data===undefined`,
    // so `filteredTotal` resolves to 0 — without the explicit loading
    // branch the empty-state chain would flash the wrong copy for the
    // duration of the first network round-trip. This test pins the
    // loading branch in place so a future refactor can't regress it.
    const { rerender, container } = renderDialog(
      {},
      pairsReturn({ isLoading: true, data: undefined }),
      pairsReturn({ isLoading: true, data: undefined }),
    );
    expect(screen.getByTestId("drilldown-loading")).toBeInTheDocument();
    // Neither empty-state card may appear during loading.
    expect(screen.queryByTestId("drilldown-empty-filters")).not.toBeInTheDocument();
    expect(screen.queryByTestId("drilldown-empty-guardrails")).not.toBeInTheDocument();

    // Once the query lands, the table renders.
    vi.mocked(useCandidatePairDetails).mockImplementation((_id, _ip, q: PairDetailsQuery) => {
      const r =
        q.limit === 0
          ? pairsReturn({ data: { pages: [pageOf([], 1)], pageParams: [null] } })
          : pairsReturn({ data: { pages: [pageOf([makeEntry(1)], 1)], pageParams: [null] } });
      return r as unknown as ReturnType<typeof useCandidatePairDetails>;
    });
    rerender(
      <DrilldownDialog
        candidate={makeCandidate()}
        campaign={makeCampaign()}
        evaluation={null}
        onClose={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("drilldown-loading")).not.toBeInTheDocument();
    expect(screen.getByTestId("candidate-pair-row-0")).toBeInTheDocument();
    expect(container).toBeTruthy();
  });

  test("filtered total = 0 with no toolbar filter shows the all-dropped-by-guardrails card", () => {
    // Both hooks return total=0 and no toolbar filter has been
    // touched, so the chain falls through to the
    // "all scored rows dropped by the active guardrails" branch.
    renderDialog(
      {},
      pairsReturn({ data: { pages: [pageOf([], 0)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 0)], pageParams: [null] } }),
    );
    expect(screen.getByTestId("drilldown-empty-guardrails")).toBeInTheDocument();
    // Sanity: the filter-active branch must not co-render.
    expect(screen.queryByTestId("drilldown-empty-filters")).not.toBeInTheDocument();
  });

  test("filtered total = 0 with an active toolbar filter shows the 'no rows match' card", async () => {
    // The internal query state defaults to DEFAULT_QUERY (no toolbar
    // filters), so `filterIsActive===false` until the operator
    // touches a control. Click the Qualifies-only switch so
    // `qualifies_only===true` and `filterIsActive===true`, which
    // routes the empty-state chain to the filter-active branch.
    renderDialog(
      {},
      pairsReturn({ data: { pages: [pageOf([], 0)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 50)], pageParams: [null] } }),
    );
    const user = userEvent.setup();
    await user.click(screen.getByTestId("filter-qualifies-only"));
    expect(screen.getByTestId("drilldown-empty-filters")).toBeInTheDocument();
    // Sanity: the guardrail-dropped branch must not co-render.
    expect(screen.queryByTestId("drilldown-empty-guardrails")).not.toBeInTheDocument();
  });

  test("network failure renders a destructive card with a Retry button", async () => {
    const refetch = vi.fn();
    renderDialog(
      {},
      pairsReturn({
        isError: true,
        error: new Error("boom"),
        refetch,
      }),
    );
    expect(screen.getByText(/failed to load pair details/i)).toBeInTheDocument();
    const retry = screen.getByRole("button", { name: /retry/i });
    const user = userEvent.setup();
    await user.click(retry);
    expect(refetch).toHaveBeenCalled();
  });

  test("404 not_a_candidate renders the destructive card with Close", () => {
    const cause: { error: string } = { error: "not_a_candidate" };
    const onClose = vi.fn();
    renderDialog(
      { onClose },
      pairsReturn({
        isError: true,
        error: Object.assign(new Error("failed"), { cause }),
      }),
    );
    expect(screen.getByText(/not a candidate/i)).toBeInTheDocument();
  });
});

describe("DrilldownDialog — table rows", () => {
  test("renders rows from the filtered hook", () => {
    renderDialog(
      {},
      pairsReturn({
        data: { pages: [pageOf([makeEntry(1), makeEntry(2)], 2)], pageParams: [null] },
      }),
      pairsReturn({ data: { pages: [pageOf([], 2)], pageParams: [null] } }),
    );
    expect(screen.getByTestId("candidate-pair-row-0")).toBeInTheDocument();
    expect(screen.getByTestId("candidate-pair-row-1")).toBeInTheDocument();
  });
});

describe("DrilldownDialog — sort toggle", () => {
  test("clicking the active default-sort header flips direction instead of looping back to the same state", async () => {
    // Default sort is improvement_ms desc. The three-state cycle's
    // "no sort" state would normally fall back to default, which on
    // the active column is identical to the current state — so the
    // operator could never reach asc by clicking. The dialog detects
    // the loopback case and flips direction instead.
    renderDialog(
      {},
      pairsReturn({ data: { pages: [pageOf([makeEntry(1)], 1)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 1)], pageParams: [null] } }),
    );

    const improvementHeader = screen.getByText("Δ ms").closest("button");
    expect(improvementHeader).not.toBeNull();

    // First click: cycle desc → null → "loopback" → flips to asc.
    const user = userEvent.setup();
    await user.click(improvementHeader as HTMLElement);

    // The Δ ms header cell's aria-sort should be ascending now.
    const improvementCell = improvementHeader?.closest("[aria-sort]");
    expect(improvementCell).toHaveAttribute("aria-sort", "ascending");

    // Second click: cycle null/asc → asc → desc.
    await user.click(improvementHeader as HTMLElement);
    expect(improvementCell).toHaveAttribute("aria-sort", "descending");
  });
});

describe("DrilldownDialog — virtualizer auto-fetch", () => {
  test("calls fetchNextPage when the virtualizer's tail row approaches the loaded set", async () => {
    // The virtualizer's `useEffect` fires synchronously after mount: with
    // `hasNextPage=true` and a small loaded set, jsdom's zero-layout
    // viewport renders every row, so the last virtual item index lands
    // at `rows.length - 1` ≥ `rows.length - 5`.
    const fetchNextPage = vi.fn();
    const entries = Array.from({ length: 8 }, (_, i) => makeEntry(i + 1));
    renderDialog(
      {},
      pairsReturn({
        data: { pages: [pageOf(entries, 8)], pageParams: [null] },
        hasNextPage: true,
        fetchNextPage,
      }),
      pairsReturn({ data: { pages: [pageOf([], 8)], pageParams: [null] } }),
    );
    await waitFor(() => {
      expect(fetchNextPage).toHaveBeenCalled();
    });
  });
});

describe("DrilldownDialog — inline MTR panel", () => {
  test("clicking an MTR icon button on a row reveals the inline MtrPanel", async () => {
    const entries = [
      makeEntry(1, {
        mtr_measurement_id_ax: 4242,
        mtr_measurement_id_xb: null,
      }),
    ];
    renderDialog(
      {},
      pairsReturn({ data: { pages: [pageOf(entries, 1)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 1)], pageParams: [null] } }),
    );

    // Inline panel is not rendered until an MTR icon is clicked.
    expect(screen.queryByRole("region", { name: /mtr hops/i })).toBeNull();

    // Each row renders one A→X button (per `MtrIconButton`'s `arrow`
    // prop). Clicking it sets `activeMtr` on the dialog body, which
    // mounts `MtrPanel` below the table.
    const user = userEvent.setup();
    const buttons = screen.getAllByRole("button", { name: /^MTR / });
    expect(buttons.length).toBeGreaterThan(0);
    await user.click(buttons[0]);

    // The MtrPanel renders a <section aria-label="MTR hops">.
    expect(await screen.findByRole("region", { name: /mtr hops/i })).toBeInTheDocument();
  });
});

describe("DrilldownDialog — re-evaluate mid-pagination", () => {
  test("page-2 fetch after a re-evaluate uses the latest evaluation snapshot", async () => {
    // Boot with page 1 loaded. The hook refetch is invoked when the
    // evaluation cache rolls forward (SSE `evaluated` invalidates the
    // pair-details query), and the auto-fetch effect re-fires against
    // the new pages on the next render.
    const refetch = vi.fn();
    const fetchNextPage = vi.fn();
    const initialEntries = Array.from({ length: 8 }, (_, i) => makeEntry(i + 1));

    let activeFiltered = pairsReturn({
      data: { pages: [pageOf(initialEntries, 8)], pageParams: [null] },
      hasNextPage: true,
      fetchNextPage,
      refetch,
    });
    let activeUnfiltered = pairsReturn({
      data: { pages: [pageOf([], 8)], pageParams: [null] },
      refetch,
    });

    vi.mocked(useCandidatePairDetails).mockImplementation((_id, _ip, q: PairDetailsQuery) => {
      const r = q.limit === 0 ? activeUnfiltered : activeFiltered;
      return r as unknown as ReturnType<typeof useCandidatePairDetails>;
    });

    vi.mocked(useAgents).mockReturnValue({
      data: [],
      isLoading: false,
      isError: false,
    } as unknown as ReturnType<typeof useAgents>);

    vi.mocked(useCampaignMeasurements).mockReturnValue({
      data: { pages: [{ entries: [], next_cursor: null }], pageParams: [null] },
      isLoading: false,
      isError: false,
      error: null,
    } as unknown as ReturnType<typeof useCampaignMeasurements>);

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

    const { rerender } = render(
      <DrilldownDialog
        candidate={makeCandidate()}
        campaign={makeCampaign()}
        evaluation={makeEvaluation()}
        onClose={vi.fn()}
      />,
      { wrapper: Wrapper },
    );

    await waitFor(() => {
      expect(fetchNextPage).toHaveBeenCalledTimes(1);
    });
    fetchNextPage.mockClear();

    // Simulate a re-evaluate: the new evaluation snapshot lands in
    // cache and React Query swaps the page set under the hooks. The
    // dialog re-renders against page 1 of the new snapshot — the
    // virtualizer's auto-fetch effect should fire again because
    // `hasNextPage` is still true and `rows.length` is small.
    const refreshedEntries = Array.from({ length: 8 }, (_, i) =>
      makeEntry(i + 1, { improvement_ms: 30 }),
    );
    activeFiltered = pairsReturn({
      data: { pages: [pageOf(refreshedEntries, 8)], pageParams: [null] },
      hasNextPage: true,
      fetchNextPage,
      refetch,
    });
    activeUnfiltered = pairsReturn({
      data: { pages: [pageOf([], 8)], pageParams: [null] },
      refetch,
    });

    await act(async () => {
      rerender(
        <DrilldownDialog
          candidate={makeCandidate()}
          campaign={makeCampaign()}
          // New evaluation snapshot — different `evaluated_at` simulates
          // an SSE-driven rollover. `useCandidatePairDetails` reads the
          // new pages above; the dialog only forwards the snapshot for
          // caption math.
          evaluation={makeEvaluation({ evaluated_at: "2026-04-21T11:00:00Z" })}
          onClose={vi.fn()}
        />,
      );
    });

    await waitFor(() => {
      expect(fetchNextPage).toHaveBeenCalled();
    });
  });
});
