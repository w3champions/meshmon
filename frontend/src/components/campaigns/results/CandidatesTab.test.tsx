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

// Stub RouteTopology to keep cytoscape out of jsdom.
vi.mock("@/components/RouteTopology", () => ({
  RouteTopology: () => <div data-testid="route-topology" />,
}));

import { useAgents } from "@/api/hooks/agents";
import { useCampaignMeasurements, useForcePair } from "@/api/hooks/campaigns";
import { useEvaluation, useTriggerDetail } from "@/api/hooks/evaluation";
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
          pair_details: [
            {
              source_agent_id: "agent-a",
              destination_agent_id: "agent-b",
              destination_ip: "10.0.0.1",
              direct_rtt_ms: 50,
              direct_stddev_ms: 2,
              direct_loss_ratio: 0.001,
              transit_rtt_ms: 20,
              transit_stddev_ms: 1,
              transit_loss_ratio: 0.0005,
              improvement_ms: 30,
              qualifies: true,
              mtr_measurement_id_ax: null,
              mtr_measurement_id_xb: null,
            },
          ],
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

  test("clicking a candidate row opens the drawer", async () => {
    setupMocks(makeEvaluation());
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "evaluated" }));

    await user.click(screen.getByTestId("candidate-row-10.0.0.1"));

    // Drawer description is drawer-unique — the candidate row shows the
    // display name, but only the drawer prints the baseline-pair summary.
    // The IP is rendered via `<IpHostname>`, which splits the text across
    // nested spans — match on concatenated textContent, scoped to the
    // leaf-ish node that owns the full string (i.e. the rendered element
    // whose `textContent` matches AND whose children don't individually
    // satisfy the match).
    expect(
      screen.getByText((_, node) => {
        if (node === null) return false;
        const re = /transit candidate 10\.0\.0\.1 — 2 of 3 baseline pairs improved/i;
        if (!re.test(node.textContent ?? "")) return false;
        const childMatches = Array.from(node.children).some((child) =>
          re.test(child.textContent ?? ""),
        );
        return !childMatches;
      }),
    ).toBeInTheDocument();
    // Pair-scoring list mounts when pair_details is non-empty.
    expect(screen.getByText(/per-pair scoring/i)).toBeInTheDocument();
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

describe("CandidatesTab — row actions", () => {
  test("force re-measure pair fires useForcePair with (source, destination) from the first pair", async () => {
    setupMocks(makeEvaluation());
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "evaluated" }));

    await user.click(screen.getByLabelText(/actions for 10\.0\.0\.1/i));
    await user.click(screen.getByText(/force re-measure pair/i));

    expect(forcePairStub.mutate).toHaveBeenCalledTimes(1);
    const [vars] = forcePairStub.mutate.mock.calls[0];
    expect(vars).toEqual({
      id: CAMPAIGN_ID,
      body: { source_agent_id: "agent-a", destination_ip: "10.0.0.1" },
    });
  });

  test("dispatch detail for pair fires useTriggerDetail with scope=pair", async () => {
    setupMocks(makeEvaluation());
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "evaluated" }));

    await user.click(screen.getByLabelText(/actions for 10\.0\.0\.1/i));
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

  test("no_pairs_selected error surfaces a dedicated toast", async () => {
    setupMocks(makeEvaluation());
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "evaluated" }));

    const err = new Error("failed", { cause: { error: "no_pairs_selected" } });
    triggerDetailStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByLabelText(/actions for 10\.0\.0\.1/i));
    await user.click(screen.getByText(/dispatch detail for this pair/i));

    await waitFor(() => {
      expect(screen.getByText(/no pairs qualified/i)).toBeInTheDocument();
    });
  });

  test("keyboard activation (Enter) on 'Force re-measure pair' fires the mutation", async () => {
    // Radix `DropdownMenuItem` uses `onSelect` — not `onClick` — so the
    // keyboard path (ArrowDown + Enter) activates the item. `onClick`
    // binds the DOM event and misses the keyboard path; this test
    // guards that regression.
    setupMocks(makeEvaluation());
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "evaluated" }));

    const trigger = screen.getByLabelText(/actions for 10\.0\.0\.1/i);
    trigger.focus();
    await user.keyboard("{Enter}");
    // Focus lands on the first menu item on open; Enter commits it.
    await user.keyboard("{Enter}");

    expect(forcePairStub.mutate).toHaveBeenCalledTimes(1);
    const [vars] = forcePairStub.mutate.mock.calls[0];
    expect(vars).toEqual({
      id: CAMPAIGN_ID,
      body: { source_agent_id: "agent-a", destination_ip: "10.0.0.1" },
    });
  });

  test("illegal_state_transition on force_pair surfaces a dedicated toast", async () => {
    setupMocks(makeEvaluation());
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "evaluated" }));

    const err = new Error("failed", { cause: { error: "illegal_state_transition" } });
    forcePairStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByLabelText(/actions for 10\.0\.0\.1/i));
    await user.click(screen.getByText(/force re-measure pair/i));

    await waitFor(() => {
      expect(screen.getByText(/campaign advanced before the request landed/i)).toBeInTheDocument();
    });
  });
});

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
