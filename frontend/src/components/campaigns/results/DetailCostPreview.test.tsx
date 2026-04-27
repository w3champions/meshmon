import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster, toast } from "sonner";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { Campaign, CampaignState } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return { ...actual, useTriggerDetail: vi.fn() };
});

import { useTriggerDetail } from "@/api/hooks/evaluation";
import {
  computeCostEstimate,
  DetailCostPreview,
} from "@/components/campaigns/results/DetailCostPreview";

const CAMPAIGN_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

function makeCampaign(overrides: Partial<Campaign> & { state: CampaignState }): Campaign {
  return {
    id: overrides.id ?? CAMPAIGN_ID,
    title: "t",
    notes: "",
    state: overrides.state,
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
    pair_counts: overrides.pair_counts ?? [
      ["succeeded", 7],
      ["reused", 3],
    ],
    max_hops: overrides.max_hops ?? 2,
    vm_lookback_minutes: overrides.vm_lookback_minutes ?? 15,
  };
}

function makeEvaluation(
  candidates: Evaluation["results"]["candidates"],
  overrides: Partial<Evaluation> = {},
): Evaluation {
  return {
    campaign_id: CAMPAIGN_ID,
    evaluated_at: "2026-04-21T10:00:00Z",
    loss_threshold_ratio: 0.02,
    stddev_weight: 1,
    evaluation_mode: "optimization",
    baseline_pair_count: 6,
    candidates_total: candidates.length,
    // Pair-detail rows live behind the paginated endpoint, so
    // candidates_good is approximated from `pairs_improved >= 1`.
    candidates_good: candidates.filter((c) => c.pairs_improved >= 1).length,
    avg_improvement_ms: 0,
    results: { candidates, unqualified_reasons: {} },
    ...overrides,
  };
}

function makeEdgeEvaluation(candidates: Evaluation["results"]["candidates"]): Evaluation {
  // edge_candidate evaluations: triple-mode counters (pairs_improved,
  // baseline_pair_count, avg_improvement_ms) are zero/unused — ranking is
  // by `coverage_count` / `coverage_weighted_ping_ms` instead.
  return {
    ...makeEvaluation(candidates),
    evaluation_mode: "edge_candidate",
    baseline_pair_count: 0,
    candidates_good: candidates.filter((c) => (c.coverage_count ?? 0) >= 1).length,
  };
}

function qualifyingCandidate(
  destinationIp: string,
  pairs_improved: number,
): Evaluation["results"]["candidates"][number] {
  return {
    destination_ip: destinationIp,
    display_name: destinationIp,
    city: null,
    country_code: null,
    asn: null,
    network_operator: null,
    is_mesh_member: false,
    pairs_improved,
    pairs_total_considered: pairs_improved,
    avg_improvement_ms: 10,
    avg_loss_ratio: 0.001,
    composite_score: 10,
  };
}

function edgeCandidate(
  destinationIp: string,
  coverage_count: number,
): Evaluation["results"]["candidates"][number] {
  // edge_candidate candidates carry coverage_count + coverage_weighted_ping_ms
  // for ranking; pairs_improved is zero (the metric is meaningless for that
  // mode) and composite_score is absent on the wire.
  return {
    destination_ip: destinationIp,
    display_name: destinationIp,
    city: null,
    country_code: null,
    asn: null,
    network_operator: null,
    is_mesh_member: false,
    pairs_improved: 0,
    pairs_total_considered: 0,
    avg_improvement_ms: null,
    avg_loss_ratio: null,
    coverage_count,
    coverage_weighted_ping_ms: 25.0,
  };
}

const triggerStub = {
  mutate: vi.fn(),
  mutateAsync: vi.fn(),
  isPending: false,
  reset: vi.fn(),
};

beforeEach(() => {
  triggerStub.mutate.mockReset();
  vi.mocked(useTriggerDetail).mockReturnValue(
    triggerStub as unknown as ReturnType<typeof useTriggerDetail>,
  );
});

afterEach(() => {
  cleanup();
  toast.dismiss();
  vi.clearAllMocks();
});

function renderDialog(props: Partial<React.ComponentProps<typeof DetailCostPreview>> = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <DetailCostPreview
        open={props.open ?? true}
        onOpenChange={props.onOpenChange ?? vi.fn()}
        campaign={props.campaign ?? makeCampaign({ state: "evaluated" })}
        scope={props.scope ?? "all"}
        pair={props.pair}
        evaluation={props.evaluation ?? null}
      />
      <Toaster />
    </QueryClientProvider>,
  );
}

// ---------------------------------------------------------------------------
// computeCostEstimate unit tests
// ---------------------------------------------------------------------------

describe("computeCostEstimate", () => {
  test("scope=all returns 2 × settled pair count", () => {
    const campaign = makeCampaign({
      state: "evaluated",
      pair_counts: [
        ["succeeded", 7],
        ["reused", 3],
        ["pending", 2],
      ],
    });
    const est = computeCostEstimate("all", campaign, null, undefined);
    expect(est.pairs_enqueued).toBe(20);
  });

  test("scope=good_candidates returns the upper-bound 4 × Σ pairs_improved", () => {
    // The candidate wire shape does not carry pair-detail rows, so the
    // preview cannot mirror the backend's exact `(agent, transit_ip)`
    // dedup — that requires fetching every page of every candidate's
    // pair-details, which is too expensive for a preview render. The
    // estimator returns
    // `4 × Σ candidate.pairs_improved` as an upper bound: each
    // qualifying triple contributes one source-side and one
    // destination-side `(agent, transit)` entry pre-dedup, each
    // expanding into ping + MTR.
    //
    // Fixture: pairs_improved totals 5 (2 + 2 + 1) — but the third
    // candidate is filtered out by the qualifying-triple test below.
    // Without the qualifying gate the upper bound is `4 × (2 + 2) = 16`.
    const candidates = [
      qualifyingCandidate("10.0.0.1", 2),
      qualifyingCandidate("10.0.0.2", 2),
      // pairs_improved = 0 ⇒ filtered out (per the backend rule).
      { ...qualifyingCandidate("10.0.0.3", 1), pairs_improved: 0 },
    ];
    const evaluation = makeEvaluation(candidates);
    const est = computeCostEstimate(
      "good_candidates",
      makeCampaign({ state: "evaluated" }),
      evaluation,
      undefined,
    );
    expect(est.pairs_enqueued).toBe(16);
  });

  test("scope=good_candidates skips candidates with pairs_improved=0", () => {
    // The backend filters `candidates.iter().filter(|c| c.pairs_improved >= 1)`
    // before expanding. A candidate with `pairs_improved=0` contributes
    // nothing to the upper-bound estimate.
    const candidates = [{ ...qualifyingCandidate("10.0.0.1", 1), pairs_improved: 0 }];
    const evaluation = makeEvaluation(candidates);
    const est = computeCostEstimate(
      "good_candidates",
      makeCampaign({ state: "evaluated" }),
      evaluation,
      undefined,
    );
    expect(est.pairs_enqueued).toBe(0);
  });

  test("scope=pair returns 2", () => {
    const est = computeCostEstimate("pair", makeCampaign({ state: "evaluated" }), null, {
      source_agent_id: "a",
      destination_ip: "10.0.0.1",
    });
    expect(est.pairs_enqueued).toBe(2);
  });

  test("scope=good_candidates branches on edge_candidate mode", () => {
    // Backend `good_candidates_for_edge_campaign` cross-joins
    // `coverage_count >= 1` candidates with the campaign's source agents,
    // and `insert_detail_pairs` enqueues a ping + MTR per pair → the
    // upper bound is `2 × source_agents × qualifying_candidates`.
    //
    // The triple-mode formula must NOT be used here: edge_candidate
    // evaluations always set `pairs_improved = 0`, so it would always
    // report zero and disable the confirm button even when there are
    // qualifying candidates ready to dispatch.
    const candidates = [
      edgeCandidate("10.0.0.1", 2), // qualifies
      edgeCandidate("10.0.0.2", 1), // qualifies
      { ...edgeCandidate("10.0.0.3", 0), coverage_count: 0 }, // filtered
    ];
    const evaluation = makeEdgeEvaluation(candidates);
    const campaign = {
      ...makeCampaign({ state: "evaluated" }),
      source_agent_ids: ["agent-a", "agent-b", "agent-c"],
      evaluation_mode: "edge_candidate" as const,
    };
    const est = computeCostEstimate("good_candidates", campaign, evaluation, undefined);
    // 3 source agents × 2 qualifying candidates × 2 (ping + MTR) = 12.
    expect(est.pairs_enqueued).toBe(12);
  });

  test("scope=good_candidates returns 0 for edge_candidate when no source agents", () => {
    // An edge_candidate campaign whose `source_agent_ids` is empty (e.g.
    // never persisted) has no agents to fan out to; the cross-join is empty.
    const evaluation = makeEdgeEvaluation([edgeCandidate("10.0.0.1", 1)]);
    const campaign = {
      ...makeCampaign({ state: "evaluated" }),
      source_agent_ids: [],
      evaluation_mode: "edge_candidate" as const,
    };
    const est = computeCostEstimate("good_candidates", campaign, evaluation, undefined);
    expect(est.pairs_enqueued).toBe(0);
  });

  test("scope=good_candidates skips edge candidates with coverage_count=0", () => {
    // `good_candidates_for_edge_campaign` filters
    // `WHERE coverage_count >= 1` — a candidate at exactly 0 contributes
    // nothing to the upper-bound estimate.
    const evaluation = makeEdgeEvaluation([edgeCandidate("10.0.0.1", 0)]);
    const campaign = {
      ...makeCampaign({ state: "evaluated" }),
      source_agent_ids: ["agent-a", "agent-b"],
      evaluation_mode: "edge_candidate" as const,
    };
    const est = computeCostEstimate("good_candidates", campaign, evaluation, undefined);
    expect(est.pairs_enqueued).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// Dialog render tests
// ---------------------------------------------------------------------------

describe("DetailCostPreview — dialog behaviour", () => {
  test("mounts with the scope title and estimated pair count", () => {
    renderDialog({ scope: "all" });
    expect(screen.getByTestId("detail-cost-preview")).toBeInTheDocument();
    expect(screen.getByTestId("cost-preview-pairs")).toHaveTextContent("20");
  });

  test("confirm disables the button while dispatching", async () => {
    triggerStub.mutate.mockImplementation(() => {
      // Intentionally leave the callback pending — the component flips
      // `inflight=true` synchronously so we can observe the disabled state.
    });
    const user = userEvent.setup();
    renderDialog({ scope: "all" });

    const confirm = screen.getByTestId("cost-preview-confirm");
    await user.click(confirm);
    expect(confirm).toBeDisabled();
  });

  test("no_evaluation error routes to the run-evaluate-first toast", async () => {
    const err = new Error("failed", { cause: { error: "no_evaluation" } });
    triggerStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );
    const user = userEvent.setup();
    // A non-null evaluation with qualifying triples keeps the confirm button
    // enabled so the server can reach its `no_evaluation` branch — the race
    // the toast is designed for (evaluation disappeared between the UI
    // rendering and the server-side dispatch).
    const evaluation = makeEvaluation([qualifyingCandidate("10.0.0.1", 1)]);
    renderDialog({ scope: "good_candidates", evaluation });

    await user.click(screen.getByTestId("cost-preview-confirm"));

    await waitFor(() => {
      expect(screen.getByText(/run evaluate first/i)).toBeInTheDocument();
    });
  });

  test("scope=good_candidates with evaluation=null shows loading label (not zero-pairs)", () => {
    renderDialog({
      scope: "good_candidates",
      evaluation: null,
      campaign: makeCampaign({ state: "evaluated" }),
    });
    const confirm = screen.getByTestId("cost-preview-confirm");
    expect(confirm).toBeDisabled();
    // The disabled label distinguishes "evaluation not loaded" from
    // "evaluation loaded, zero qualifying pairs" — both numerically enqueue 0,
    // but the operator's next step differs.
    expect(confirm).toHaveTextContent(/loading evaluation/i);
    expect(confirm).not.toHaveTextContent(/enqueue 0/i);
    // Description copy should not steer toward "Run Evaluate first" when the
    // evaluation is simply still loading.
    expect(screen.queryByText(/Run Evaluate first/i)).not.toBeInTheDocument();
  });

  test("scope=good_candidates with zero qualifying triples shows no-pairs label", () => {
    // pairs_improved = 0 ⇒ candidate is filtered out of the upper-bound
    // estimate, mirroring the backend's `pairs_improved >= 1` gate.
    const evaluation = makeEvaluation([
      { ...qualifyingCandidate("10.0.0.1", 1), pairs_improved: 0 },
    ]);
    renderDialog({
      scope: "good_candidates",
      evaluation,
      campaign: makeCampaign({ state: "evaluated" }),
    });
    const confirm = screen.getByTestId("cost-preview-confirm");
    expect(confirm).toBeDisabled();
    expect(confirm).toHaveTextContent(/no pairs to enqueue/i);
    // And the description explains the *why* (not a loading message).
    expect(screen.getByText(/no qualifying pairs/i)).toBeInTheDocument();
  });

  test("scope=good_candidates with edge_candidate evaluation enables confirm button", () => {
    // Regression: the triple-mode formula uses `pairs_improved`, which is
    // always 0 in edge_candidate evaluations — so the dialog used to render
    // `≤ 0` and disable the confirm button even when there are coverage
    // candidates to dispatch. The mode-aware formula must report the
    // cross-join `2 × source_agents × qualifying_candidates` instead.
    const evaluation = makeEdgeEvaluation([
      edgeCandidate("10.0.0.1", 2),
      edgeCandidate("10.0.0.2", 1),
    ]);
    const campaign = {
      ...makeCampaign({ state: "evaluated" }),
      source_agent_ids: ["agent-a", "agent-b"],
      evaluation_mode: "edge_candidate" as const,
    };
    renderDialog({ scope: "good_candidates", evaluation, campaign });

    // 2 agents × 2 qualifying × 2 (ping + MTR) = 8.
    expect(screen.getByTestId("cost-preview-pairs")).toHaveTextContent("8");
    const confirm = screen.getByTestId("cost-preview-confirm");
    expect(confirm).not.toBeDisabled();
    expect(confirm).toHaveTextContent(/enqueue 8/i);
  });

  test("scope=pair body carries the supplied pair identifier", async () => {
    const user = userEvent.setup();
    renderDialog({
      scope: "pair",
      pair: { source_agent_id: "agent-a", destination_ip: "10.0.0.1" },
    });

    await user.click(screen.getByTestId("cost-preview-confirm"));

    expect(triggerStub.mutate).toHaveBeenCalledTimes(1);
    const [vars] = triggerStub.mutate.mock.calls[0];
    expect(vars).toEqual({
      id: CAMPAIGN_ID,
      body: {
        scope: "pair",
        pair: { source_agent_id: "agent-a", destination_ip: "10.0.0.1" },
      },
    });
  });
});
