import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster, toast } from "sonner";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { Campaign, CampaignState } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";
import { IpHostnameProvider } from "@/components/ip-hostname";

// ---------------------------------------------------------------------------
// Module mocks
// ---------------------------------------------------------------------------

const navigate = vi.fn();
vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return {
    ...actual,
    useNavigate: () => navigate,
    useSearch: () => ({ tab: "candidates" }),
  };
});

vi.mock("@/api/hooks/agents", async () => {
  const actual = await vi.importActual<typeof import("@/api/hooks/agents")>("@/api/hooks/agents");
  return { ...actual, useAgents: vi.fn() };
});

vi.mock("@/api/hooks/campaigns", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/campaigns")>("@/api/hooks/campaigns");
  return {
    ...actual,
    useForcePair: vi.fn(),
    useCampaignMeasurements: vi.fn(),
  };
});

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return { ...actual, useEvaluation: vi.fn(), useTriggerDetail: vi.fn() };
});

vi.mock("@/api/hooks/evaluation-pairs", async () => {
  const actual = await vi.importActual<typeof import("@/api/hooks/evaluation-pairs")>(
    "@/api/hooks/evaluation-pairs",
  );
  return { ...actual, useCandidatePairDetails: vi.fn() };
});

// Stub RouteTopology to keep cytoscape out of jsdom.
vi.mock("@/components/RouteTopology", () => ({
  RouteTopology: () => <div data-testid="route-topology" />,
}));

import { useAgents } from "@/api/hooks/agents";
import { useCampaignMeasurements, useForcePair } from "@/api/hooks/campaigns";
import { useEvaluation, useTriggerDetail } from "@/api/hooks/evaluation";
import { useCandidatePairDetails } from "@/api/hooks/evaluation-pairs";
import { CandidatesTab } from "@/components/campaigns/results/CandidatesTab";

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
  };
}

function makeEvaluation(): Evaluation {
  return {
    campaign_id: CAMPAIGN_ID,
    evaluated_at: "2026-04-21T10:00:00Z",
    loss_threshold_ratio: 0.02,
    stddev_weight: 1,
    evaluation_mode: "optimization",
    baseline_pair_count: 6,
    candidates_total: 2,
    candidates_good: 1,
    avg_improvement_ms: 25,
    results: {
      candidates: [
        {
          destination_ip: "10.0.0.1",
          display_name: "candidate-one",
          city: "Berlin",
          country_code: "DE",
          asn: 12345,
          network_operator: "ExampleNet",
          is_mesh_member: false,
          pairs_improved: 2,
          pairs_total_considered: 3,
          avg_improvement_ms: 30,
          avg_loss_ratio: 0.001,
          composite_score: 20,
        },
      ],
      unqualified_reasons: { "192.168.1.1": "Transit path exceeded loss threshold." },
    },
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

function setupMocks(evaluation: Evaluation | null, opts?: { isLoading?: boolean }) {
  vi.mocked(useEvaluation).mockReturnValue({
    data: evaluation,
    isLoading: opts?.isLoading ?? false,
    isError: false,
    error: null,
  } as unknown as ReturnType<typeof useEvaluation>);

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
  vi.mocked(useCampaignMeasurements).mockReturnValue({
    data: { pages: [{ entries: [], next_cursor: null }], pageParams: [null] },
    isLoading: false,
    isError: false,
    error: null,
  } as unknown as ReturnType<typeof useCampaignMeasurements>);
  vi.mocked(useCandidatePairDetails).mockReturnValue({
    data: { pages: [{ entries: [], next_cursor: null, total: 0 }], pageParams: [null] },
    isLoading: false,
    isError: false,
    isFetchingNextPage: false,
    hasNextPage: false,
    error: null,
    fetchNextPage: vi.fn(),
    refetch: vi.fn(),
  } as unknown as ReturnType<typeof useCandidatePairDetails>);
}

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

function renderTab(campaign: Campaign) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <IpHostnameProvider>
        <CandidatesTab campaign={campaign} />
        <Toaster />
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
  forcePairStub.mutate.mockReset();
  triggerDetailStub.mutate.mockReset();
  navigate.mockReset();
});

afterEach(() => {
  cleanup();
  toast.dismiss();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("CandidatesTab — loading + empty", () => {
  test("renders a skeleton while evaluation is loading", () => {
    setupMocks(null, { isLoading: true });
    renderTab(makeCampaign({ state: "completed" }));
    expect(screen.getByTestId("candidates-tab")).toHaveAttribute("role", "status");
  });

  test("renders the 'no evaluation yet' empty state with a Settings CTA", async () => {
    setupMocks(null);
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    expect(screen.getByText(/no evaluation yet/i)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /open settings tab/i }));
    expect(navigate).toHaveBeenCalledWith({
      search: { tab: "settings" },
      replace: true,
    });
  });
});

describe("CandidatesTab — happy path", () => {
  test("mounts the KPI strip + CandidateTable when an evaluation exists", () => {
    setupMocks(makeEvaluation());
    renderTab(makeCampaign({ state: "evaluated" }));

    // KPI strip
    expect(screen.getByText("Baseline pairs")).toBeInTheDocument();
    expect(screen.getByText("6")).toBeInTheDocument();
    expect(screen.getByText("1 / 2")).toBeInTheDocument();
    // Candidate row
    expect(screen.getByTestId("candidate-row-10.0.0.1")).toBeInTheDocument();
  });

  test("clicking a candidate row opens the dialog", async () => {
    setupMocks(makeEvaluation());
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "evaluated" }));

    await user.click(screen.getByTestId("candidate-row-10.0.0.1"));

    // The dialog renders the candidate's headline counters in its
    // description ("X of Y baseline pairs improved"). Using
    // findByTestId waits for the dialog body to mount once the
    // pair-details fetch is in flight (the body is gated on the
    // candidate prop being non-null, which the click installs).
    expect(await screen.findByTestId("drilldown-body")).toBeInTheDocument();
    // The filter toolbar mounts inside the dialog.
    expect(screen.getByTestId("filter-min-improvement-ms")).toBeInTheDocument();
  });

  test("unqualified reasons surface in the tab body when none is selected", () => {
    setupMocks(makeEvaluation());
    renderTab(makeCampaign({ state: "evaluated" }));

    expect(screen.getByText(/unqualified candidates/i)).toBeInTheDocument();
    expect(screen.getByText(/transit path exceeded loss threshold/i)).toBeInTheDocument();
  });
});

describe("CandidatesTab — overflow menu state gating", () => {
  test("'Detail: good candidates only' is disabled on completed campaigns", async () => {
    setupMocks(makeEvaluation());
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await waitFor(() => expect(screen.getByTestId("overflow-detail-good")).toBeInTheDocument());
    expect(screen.getByTestId("overflow-detail-good")).toHaveAttribute("aria-disabled", "true");
  });

  test("'Detail: good candidates only' is enabled on evaluated campaigns", async () => {
    setupMocks(makeEvaluation());
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "evaluated" }));

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await waitFor(() => expect(screen.getByTestId("overflow-detail-good")).toBeInTheDocument());
    expect(screen.getByTestId("overflow-detail-good")).toHaveAttribute("aria-disabled", "false");
  });
});

// T55: per-row force-pair / dispatch-pair actions moved into the
// drilldown dialog. The tab itself no longer renders a per-row action
// menu — the action requires a `(source_agent_id, destination_ip)`
// tuple that is reachable only via the paginated pair-details endpoint
// the dialog already fetches.

describe("CandidatesTab — sort URL state", () => {
  test("sort header click emits navigate with cand_sort + cand_dir", () => {
    setupMocks(makeEvaluation());
    renderTab(makeCampaign({ state: "evaluated" }));

    // The default sort col is `composite_score` desc. Clicking Score should
    // flip to asc.
    fireEvent.click(screen.getByRole("button", { name: /score/i }));
    expect(navigate).toHaveBeenCalledWith({
      search: {
        tab: "candidates",
        cand_sort: "composite_score",
        cand_dir: "asc",
      },
      replace: true,
    });
  });
});
