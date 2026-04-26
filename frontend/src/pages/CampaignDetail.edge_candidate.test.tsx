/**
 * Page-level integration tests for CampaignDetail with evaluation_mode =
 * "edge_candidate". These tests mount the REAL tab-body components (CandidatesTab,
 * HeatmapTab, PairsTab, CompareTab, SettingsTab, RawTab) and only mock the data
 * layer (TanStack Query hooks / network calls). The goal is to verify the
 * end-to-end edge_candidate flow renders and responds to user interactions.
 */
import "@testing-library/jest-dom/vitest";
import React from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { Campaign, CampaignState } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";

// ---------------------------------------------------------------------------
// Module mocks — register BEFORE importing the component under test.
// ---------------------------------------------------------------------------

const navigate = vi.fn();
vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return { ...actual, useNavigate: () => navigate };
});

vi.mock("@/api/hooks/campaigns", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/campaigns")>("@/api/hooks/campaigns");
  return {
    ...actual,
    useCampaign: vi.fn(),
    useCampaignPairs: vi.fn(),
    usePreviewDispatchCount: vi.fn(),
    useStartCampaign: vi.fn(),
    useStopCampaign: vi.fn(),
    useDeleteCampaign: vi.fn(),
    useEditCampaign: vi.fn(),
    usePatchCampaign: vi.fn(),
    useForcePair: vi.fn(),
  };
});

vi.mock("@/api/hooks/campaign-stream", () => ({
  useCampaignStream: vi.fn(),
}));

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return {
    ...actual,
    useEvaluation: vi.fn(),
    useEvaluateCampaign: vi.fn(),
    useEdgePairDetails: vi.fn(),
    useTriggerDetail: vi.fn(),
  };
});

vi.mock("@/api/hooks/agents", () => ({
  useAgents: vi.fn(),
}));

vi.mock("@/stores/toast", () => ({
  useToastStore: { getState: () => ({ pushToast: vi.fn() }) },
}));

vi.mock("@/components/campaigns/EditMetadataSheet", () => ({
  EditMetadataSheet: ({ open }: { open: boolean }) =>
    open ? <div data-testid="edit-metadata-sheet" /> : null,
}));

vi.mock("@/components/catalogue/CatalogueDrawerOverlay", () => ({
  CatalogueDrawerOverlay: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="catalogue-drawer-overlay">{children}</div>
  ),
  useCatalogueDrawer: () => ({ open: vi.fn() }),
}));

// Stub EventSource for ip-hostname provider
class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

// ---------------------------------------------------------------------------
// Imports AFTER mocks.
// ---------------------------------------------------------------------------

import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  useCampaign,
  useCampaignPairs,
  useDeleteCampaign,
  useEditCampaign,
  useForcePair,
  usePatchCampaign,
  usePreviewDispatchCount,
  useStartCampaign,
  useStopCampaign,
} from "@/api/hooks/campaigns";
import { useAgents } from "@/api/hooks/agents";
import {
  useEdgePairDetails,
  useEvaluateCampaign,
  useEvaluation,
  useTriggerDetail,
  type EvaluationEdgePairDetailDto,
} from "@/api/hooks/evaluation";
import { IpHostnameProvider } from "@/components/ip-hostname";
import CampaignDetail from "@/pages/CampaignDetail";
import { campaignDetailSearchSchema } from "@/router/index";
import { useComposerSeedStore } from "@/stores/composer-seed";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";

function makeCampaign(overrides: Partial<Campaign> & { state: CampaignState }): Campaign {
  return {
    id: overrides.id ?? CAMPAIGN_ID,
    title: overrides.title ?? "Edge Campaign",
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
    vm_lookback_minutes: overrides.vm_lookback_minutes ?? 15,
    useful_latency_ms: overrides.useful_latency_ms ?? 80,
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
    useful_latency_ms: 80,
    max_hops: 2,
    vm_lookback_minutes: 15,
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
    is_unreachable: false,
  };
}

function makeEdgePairQueryResult(rows: EvaluationEdgePairDetailDto[]) {
  return {
    data: {
      pages: [{ entries: rows, next_cursor: null, total: rows.length }],
      pageParams: [null],
    },
    isLoading: false,
    isError: false,
    error: null,
    isFetchingNextPage: false,
    hasNextPage: false,
    fetchNextPage: vi.fn(),
    refetch: vi.fn(),
  };
}

function makeMutationStub() {
  return { mutate: vi.fn(), mutateAsync: vi.fn(), isPending: false, reset: vi.fn() };
}

const startStub = makeMutationStub();
const stopStub = makeMutationStub();
const deleteStub = makeMutationStub();
const editStub = makeMutationStub();
const patchStub = makeMutationStub();
const evaluateStub = makeMutationStub();
const forcePairStub = makeMutationStub();
const triggerDetailStub = makeMutationStub();

interface HookSetupOptions {
  campaign?: Campaign | null;
  isLoading?: boolean;
  isError?: boolean;
  evaluation?: Evaluation | null;
  edgePairRows?: EvaluationEdgePairDetailDto[];
}

function setupHookMocks(opts: HookSetupOptions = {}) {
  vi.mocked(useCampaign).mockReturnValue({
    data: opts.campaign ?? null,
    isLoading: opts.isLoading ?? false,
    isError: opts.isError ?? false,
    error: null,
    refetch: vi.fn(),
  } as unknown as ReturnType<typeof useCampaign>);

  vi.mocked(useCampaignPairs).mockReturnValue({
    data: [],
    isLoading: false,
    isError: false,
    refetch: vi.fn(),
  } as unknown as ReturnType<typeof useCampaignPairs>);

  vi.mocked(usePreviewDispatchCount).mockReturnValue({
    data: { fresh: 0, reusable: 0, total: 0 },
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof usePreviewDispatchCount>);

  vi.mocked(useStartCampaign).mockReturnValue(
    startStub as unknown as ReturnType<typeof useStartCampaign>,
  );
  vi.mocked(useStopCampaign).mockReturnValue(
    stopStub as unknown as ReturnType<typeof useStopCampaign>,
  );
  vi.mocked(useDeleteCampaign).mockReturnValue(
    deleteStub as unknown as ReturnType<typeof useDeleteCampaign>,
  );
  vi.mocked(useEditCampaign).mockReturnValue(
    editStub as unknown as ReturnType<typeof useEditCampaign>,
  );
  vi.mocked(usePatchCampaign).mockReturnValue(
    patchStub as unknown as ReturnType<typeof usePatchCampaign>,
  );
  vi.mocked(useEvaluateCampaign).mockReturnValue(
    evaluateStub as unknown as ReturnType<typeof useEvaluateCampaign>,
  );
  vi.mocked(useForcePair).mockReturnValue(
    forcePairStub as unknown as ReturnType<typeof useForcePair>,
  );
  vi.mocked(useTriggerDetail).mockReturnValue(
    triggerDetailStub as unknown as ReturnType<typeof useTriggerDetail>,
  );

  vi.mocked(useCampaignStream).mockReturnValue(undefined);

  vi.mocked(useEvaluation).mockReturnValue({
    data: opts.evaluation !== undefined ? opts.evaluation : null,
    isLoading: false,
    isError: false,
    error: null,
  } as unknown as ReturnType<typeof useEvaluation>);

  const edgeRows = opts.edgePairRows ?? [];
  vi.mocked(useEdgePairDetails).mockReturnValue(
    makeEdgePairQueryResult(edgeRows) as unknown as ReturnType<typeof useEdgePairDetails>,
  );

  vi.mocked(useAgents).mockReturnValue({
    data: [],
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useAgents>);
}

// ---------------------------------------------------------------------------
// Router harness
// ---------------------------------------------------------------------------

interface RenderOptions {
  search?: string;
}

function renderDetail(opts: RenderOptions = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });

  const rootRoute = createRootRoute({ component: Outlet });
  const detailRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns/$id",
    component: CampaignDetail,
    validateSearch: (raw: Record<string, unknown>) => {
      const result = campaignDetailSearchSchema.safeParse(raw);
      const parsed = result.success ? result.data : { tab: "candidates" as const };
      return {
        tab: parsed.tab ?? ("candidates" as const),
        raw_state: parsed.raw_state,
        raw_protocol: parsed.raw_protocol,
        raw_kind: parsed.raw_kind,
      };
    },
  });
  const listRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns",
    component: () => null,
  });

  const router = createRouter({
    routeTree: rootRoute.addChildren([detailRoute, listRoute]),
    history: createMemoryHistory({
      initialEntries: [`/campaigns/${CAMPAIGN_ID}${opts.search ?? ""}`],
    }),
  });

  const result = render(
    <QueryClientProvider client={client}>
      <IpHostnameProvider>
        <RouterProvider router={router} />
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
  return { ...result, router, client };
}

// ---------------------------------------------------------------------------
// Setup / teardown
// ---------------------------------------------------------------------------

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
  localStorage.clear();
  navigate.mockReset();
  startStub.mutate.mockReset();
  stopStub.mutate.mockReset();
  deleteStub.mutate.mockReset();
  editStub.mutate.mockReset();
  patchStub.mutate.mockReset();
  evaluateStub.mutate.mockReset();
  forcePairStub.mutate.mockReset();
  triggerDetailStub.mutate.mockReset();
  useComposerSeedStore.setState({ seed: null });
  setupHookMocks();
});

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
  localStorage.clear();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("CampaignDetail edge_candidate — tab shell", () => {
  test("all 6 tab triggers render for edge_candidate evaluated campaign", async () => {
    const evaluation = makeEvaluation();
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation,
    });
    renderDetail();

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByRole("tab", { name: /candidates/i })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /heatmap/i })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /pairs/i })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /^compare$/i })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /raw measurements/i })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /evaluation settings/i })).toBeInTheDocument();
  });
});

describe("CampaignDetail edge_candidate — Heatmap tab", () => {
  test("HeatmapTab body renders with canned edge pair rows", async () => {
    const evaluation = makeEvaluation();
    const rows = [
      makeEdgePairRow("10.0.0.1", "agent-a", 42),
      makeEdgePairRow("10.0.0.1", "agent-b", 75),
    ];
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation,
      edgePairRows: rows,
    });
    renderDetail({ search: "?tab=heatmap" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByTestId("heatmap-tab")).toBeInTheDocument();
    expect(screen.getByTestId("heatmap-col-header-10.0.0.1")).toBeInTheDocument();
    expect(screen.getByTestId("heatmap-row-agent-a")).toBeInTheDocument();
    expect(screen.getByTestId("heatmap-row-agent-b")).toBeInTheDocument();
  });

  test("HeatmapTab shows 'Evaluate first' placeholder when evaluation is null", async () => {
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation: null,
    });
    renderDetail({ search: "?tab=heatmap" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByText(/evaluate first/i)).toBeInTheDocument();
    expect(screen.queryByTestId("heatmap-tab")).not.toBeInTheDocument();
  });

  test("HeatmapTab color editor is reachable by scrolling to settings section", async () => {
    const evaluation = makeEvaluation();
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 42)];
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation,
      edgePairRows: rows,
    });
    renderDetail({ search: "?tab=heatmap" });

    await screen.findByTestId("heatmap-tab");

    expect(screen.getByTestId("hm-color-editor-trigger")).toBeInTheDocument();
  });
});

describe("CampaignDetail edge_candidate — Compare tab", () => {
  test("CompareTab body renders with agent picker visible", async () => {
    const evaluation = makeEvaluation();
    const campaign = {
      ...makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      source_agent_ids: ["agent-x", "agent-y"],
    } as Campaign;
    const rows = [makeEdgePairRow("10.0.0.1", "agent-x", 50)];

    setupHookMocks({ campaign, evaluation, edgePairRows: rows });
    renderDetail({ search: "?tab=compare" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByTestId("compare-view")).toBeInTheDocument();
    expect(screen.getByTestId("agent-picker-agent-x")).toBeInTheDocument();
    expect(screen.getByTestId("agent-picker-agent-y")).toBeInTheDocument();
  });

  test("checking an agent in CompareTab updates selection (interactive)", async () => {
    const evaluation = makeEvaluation();
    const campaign = {
      ...makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      source_agent_ids: ["agent-x"],
    } as Campaign;
    const rows = [makeEdgePairRow("10.0.0.1", "agent-x", 50)];

    setupHookMocks({ campaign, evaluation, edgePairRows: rows });
    const user = userEvent.setup();
    renderDetail({ search: "?tab=compare" });

    await screen.findByTestId("compare-view");

    const checkbox = screen.getByTestId("agent-picker-agent-x");
    await user.click(checkbox);

    await waitFor(() => {
      expect(checkbox).toBeChecked();
    });
  });
});

describe("CampaignDetail edge_candidate — Pairs tab", () => {
  test("EdgePairsTab renders inside the Pairs tab for edge_candidate mode", async () => {
    const evaluation = makeEvaluation();
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 40)];

    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation,
      edgePairRows: rows,
    });
    renderDetail({ search: "?tab=pairs" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(
      screen.getByRole("columnheader", { name: /candidate/i }),
    ).toBeInTheDocument();
  });
});

describe("CampaignDetail edge_candidate — Settings tab", () => {
  test("SettingsTab renders with mode-aware controls visible", async () => {
    const evaluation = makeEvaluation();
    setupHookMocks({
      campaign: makeCampaign({
        state: "evaluated",
        evaluation_mode: "edge_candidate",
        useful_latency_ms: 80,
      }),
      evaluation,
    });
    renderDetail({ search: "?tab=settings" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByRole("group", { name: /evaluation mode/i })).toBeInTheDocument();

    const edgeCandidateToggle = screen.getByRole("radio", { name: /edge candidate/i });
    expect(edgeCandidateToggle).toBeInTheDocument();

    expect(screen.getByLabelText(/useful latency/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/lookback window/i)).toBeInTheDocument();
  });

  test("Direct only (0 hops) option is visible in SettingsTab for edge_candidate", async () => {
    const evaluation = makeEvaluation();
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation,
    });
    renderDetail({ search: "?tab=settings" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    await waitFor(() => {
      expect(screen.getByRole("radio", { name: /direct only/i })).toBeInTheDocument();
    });
  });

  test("min_improvement inputs are hidden in edge_candidate SettingsTab", async () => {
    const evaluation = makeEvaluation();
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation,
    });
    renderDetail({ search: "?tab=settings" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    await waitFor(() => {
      expect(screen.queryByLabelText(/min improvement \(ms\)/i)).not.toBeInTheDocument();
      expect(screen.queryByLabelText(/min improvement ratio/i)).not.toBeInTheDocument();
    });
  });

  test("Re-evaluate fires PATCH then POST /evaluate in sequence", async () => {
    const evaluation = makeEvaluation();
    setupHookMocks({
      campaign: makeCampaign({
        state: "evaluated",
        evaluation_mode: "edge_candidate",
        useful_latency_ms: 80,
      }),
      evaluation,
    });

    patchStub.mutate.mockImplementation(
      (_vars: unknown, handlers?: { onSuccess?: () => void }) => {
        handlers?.onSuccess?.();
      },
    );

    const user = userEvent.setup();
    renderDetail({ search: "?tab=settings" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    const reEvaluateBtn = await screen.findByRole("button", { name: /re-evaluate/i });
    await user.click(reEvaluateBtn);

    await waitFor(() => {
      expect(patchStub.mutate).toHaveBeenCalledWith(
        expect.objectContaining({
          id: CAMPAIGN_ID,
          body: expect.objectContaining({ evaluation_mode: "edge_candidate" }),
        }),
        expect.any(Object),
      );
    });

    await waitFor(() => {
      expect(evaluateStub.mutate).toHaveBeenCalledWith(CAMPAIGN_ID, expect.any(Object));
    });
  });
});

describe("CampaignDetail edge_candidate — Candidates tab + DrilldownDialog", () => {
  test("clicking a candidate row in EdgeCandidateTable opens the DrilldownDialog", async () => {
    const evaluation = makeEvaluation();
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation,
    });
    const user = userEvent.setup();
    renderDetail();

    await screen.findByRole("heading", { name: /edge campaign/i });

    const candidateRow = await screen.findByTestId("edge-candidate-row-10.0.0.1");
    await user.click(candidateRow);

    await waitFor(() => {
      expect(screen.getByRole("dialog")).toBeInTheDocument();
    });
  });
});

describe("CampaignDetail edge_candidate — Knobs card", () => {
  test("Knobs card surfaces useful_latency_ms / max_hops / vm_lookback_minutes for edge_candidate", async () => {
    const evaluation = makeEvaluation();
    setupHookMocks({
      campaign: makeCampaign({
        state: "evaluated",
        evaluation_mode: "edge_candidate",
        useful_latency_ms: 80,
        max_hops: 2,
        vm_lookback_minutes: 15,
      }),
      evaluation,
    });
    renderDetail();

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByText(/useful latency \(ms\)/i)).toBeInTheDocument();
    expect(screen.getByText(/max hops/i)).toBeInTheDocument();
    expect(screen.getByText(/lookback window \(min\)/i)).toBeInTheDocument();
  });
});

describe("CampaignDetail edge_candidate — single-source-agent banner", () => {
  test("single source agent banner appears in SettingsTab when only one source agent", async () => {
    const evaluation = makeEvaluation();
    const campaign = {
      ...makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      source_agent_ids: ["sole-agent"],
    } as Campaign;

    setupHookMocks({ campaign, evaluation });
    renderDetail({ search: "?tab=settings" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    await waitFor(() => {
      expect(screen.getByText(/only one source agent/i)).toBeInTheDocument();
    });
  });
});

describe("CampaignDetail edge_candidate — stale evaluation gating", () => {
  /**
   * Regression for the C3-9 dismissal path. After a knob change, the
   * `campaign_evaluations` row is preserved (state flips to `completed`,
   * `evaluated_at` clears) and `GET /evaluation` keeps returning the
   * historical snapshot. Heatmap and Pairs must show their placeholders
   * until the operator re-runs `/evaluate` and the campaign returns to
   * `evaluated`.
   */
  test("Heatmap shows placeholder when state is completed even if evaluation row exists", async () => {
    setupHookMocks({
      campaign: makeCampaign({ state: "completed", evaluation_mode: "edge_candidate" }),
      evaluation: makeEvaluation(),
    });
    renderDetail({ search: "?tab=heatmap" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByText(/evaluate first/i)).toBeInTheDocument();
    expect(screen.queryByTestId("heatmap-tab")).not.toBeInTheDocument();
  });

  test("Pairs shows edge placeholder when state is completed even if evaluation row exists", async () => {
    setupHookMocks({
      campaign: makeCampaign({ state: "completed", evaluation_mode: "edge_candidate" }),
      evaluation: makeEvaluation(),
    });
    renderDetail({ search: "?tab=pairs" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByTestId("edge-pairs-placeholder")).toBeInTheDocument();
  });

  /**
   * Candidates is the default tab and shares the same staleness exposure.
   * After a knob change the `campaign_evaluations` row is preserved and
   * `GET /evaluation` keeps returning the historical snapshot — the tab
   * must render its evaluate-first placeholder, not the stale candidate
   * rows. Mirrors the Heatmap/Pairs/Compare gate.
   */
  test("Candidates shows placeholder when state is completed even if evaluation row exists", async () => {
    setupHookMocks({
      campaign: makeCampaign({ state: "completed", evaluation_mode: "edge_candidate" }),
      evaluation: makeEvaluation(),
    });
    renderDetail();

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByText(/no evaluation yet/i)).toBeInTheDocument();
    expect(screen.queryByTestId("edge-candidate-row-10.0.0.1")).not.toBeInTheDocument();
  });

  /**
   * Mode-mismatch path for Candidates: the historical snapshot's mode no
   * longer matches the campaign's current mode, so the rows describe a
   * different scoring lens. Render the placeholder even though the
   * campaign is in `evaluated` state.
   */
  test("Candidates shows placeholder when evaluation_mode mismatches campaign mode", async () => {
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation: makeEvaluation({ evaluation_mode: "optimization" }),
    });
    renderDetail();

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByText(/no evaluation yet/i)).toBeInTheDocument();
    expect(screen.queryByTestId("edge-candidate-row-10.0.0.1")).not.toBeInTheDocument();
  });

  /**
   * Mode-mismatch guard. Campaign was previously evaluated in
   * `optimization` mode then PATCHed to `edge_candidate`; the historical
   * row still exists but its `evaluation_mode` no longer matches. The
   * tabs must not render that row as if it described the current mode.
   */
  test("Heatmap shows placeholder when evaluation_mode mismatches campaign mode", async () => {
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation: makeEvaluation({ evaluation_mode: "optimization" }),
    });
    renderDetail({ search: "?tab=heatmap" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByText(/evaluate first/i)).toBeInTheDocument();
    expect(screen.queryByTestId("heatmap-tab")).not.toBeInTheDocument();
  });

  /**
   * Sanity counterpart: when state is `evaluated` and the evaluation
   * snapshot's mode matches the campaign mode, the rich tab bodies render
   * as before.
   */
  test("Heatmap renders normally when state=evaluated and modes match", async () => {
    const rows = [makeEdgePairRow("10.0.0.1", "agent-a", 42)];
    setupHookMocks({
      campaign: makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      evaluation: makeEvaluation(),
      edgePairRows: rows,
    });
    renderDetail({ search: "?tab=heatmap" });

    await screen.findByRole("heading", { name: /edge campaign/i });

    expect(screen.getByTestId("heatmap-tab")).toBeInTheDocument();
    expect(screen.queryByText(/evaluate first/i)).not.toBeInTheDocument();
  });
});
