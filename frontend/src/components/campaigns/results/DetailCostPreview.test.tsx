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
  };
}

function makeEvaluation(candidates: Evaluation["results"]["candidates"]): Evaluation {
  return {
    campaign_id: CAMPAIGN_ID,
    evaluated_at: "2026-04-21T10:00:00Z",
    loss_threshold_ratio: 0.02,
    stddev_weight: 1,
    evaluation_mode: "optimization",
    baseline_pair_count: 6,
    candidates_total: candidates.length,
    candidates_good: candidates.filter((c) => c.pair_details.some((pd) => pd.qualifies)).length,
    avg_improvement_ms: 0,
    results: { candidates, unqualified_reasons: {} },
  };
}

function qualifyingCandidate(
  destinationIp: string,
  triples: Array<[string, string]>,
  qualifies: boolean = true,
): Evaluation["results"]["candidates"][number] {
  return {
    destination_ip: destinationIp,
    display_name: destinationIp,
    city: null,
    country_code: null,
    asn: null,
    network_operator: null,
    is_mesh_member: false,
    pairs_improved: triples.length,
    pairs_total_considered: triples.length,
    avg_improvement_ms: 10,
    avg_loss_ratio: 0.001,
    composite_score: 10,
    pair_details: triples.map(([src, dst]) => ({
      source_agent_id: src,
      destination_agent_id: dst,
      destination_ip: destinationIp,
      direct_rtt_ms: 50,
      direct_stddev_ms: 2,
      direct_loss_ratio: 0.001,
      transit_rtt_ms: 20,
      transit_stddev_ms: 1,
      transit_loss_ratio: 0.0005,
      improvement_ms: 30,
      qualifies,
      mtr_measurement_id_ax: null,
      mtr_measurement_id_xb: null,
    })),
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

  test("scope=good_candidates mirrors the backend's (agent, transit_ip) dedup", () => {
    // The backend's `POST /detail` handler appends `(source, transit)`
    // and `(destination_agent, transit)` per qualifying triple, then
    // sort+dedupes on that tuple (not on triple identity) before
    // expanding into ping+MTR rows. Two triples sharing an agent against
    // the same transit must collapse to one (agent, transit) entry.
    //
    // Fixture:
    //  - candidate-one (transit=10.0.0.1): triples (a→b), (c→d)
    //      contributes {(a, .1), (b, .1), (c, .1), (d, .1)} = 4 entries
    //  - candidate-two (transit=10.0.0.2): triples (a→b), (a→d)
    //      contributes {(a, .2), (b, .2), (d, .2)} = 3 entries
    //      (agent `a` against transit .2 appears in both triples → dedup)
    //  - candidate-three (transit=10.0.0.3): one unqualified triple → 0 entries
    // Total deduped = 7 entries × 2 measurements (ping+MTR) = 14.
    const candidates = [
      qualifyingCandidate("10.0.0.1", [
        ["agent-a", "agent-b"],
        ["agent-c", "agent-d"],
      ]),
      qualifyingCandidate("10.0.0.2", [
        ["agent-a", "agent-b"],
        ["agent-a", "agent-d"],
      ]),
      qualifyingCandidate("10.0.0.3", [["agent-a", "agent-b"]], false),
    ];
    const evaluation = makeEvaluation(candidates);
    const est = computeCostEstimate(
      "good_candidates",
      makeCampaign({ state: "evaluated" }),
      evaluation,
      undefined,
    );
    expect(est.pairs_enqueued).toBe(14);
  });

  test("scope=good_candidates skips candidates with pairs_improved=0", () => {
    // The backend filters `candidates.iter().filter(|c| c.pairs_improved >= 1)`
    // before expanding. A candidate with qualifying pair_details but
    // `pairs_improved=0` must be ignored.
    const candidates = [
      {
        ...qualifyingCandidate("10.0.0.1", [["agent-a", "agent-b"]]),
        pairs_improved: 0,
      },
    ];
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
    const evaluation = makeEvaluation([qualifyingCandidate("10.0.0.1", [["agent-a", "agent-b"]])]);
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
    const evaluation = makeEvaluation([
      qualifyingCandidate("10.0.0.1", [["agent-a", "agent-b"]], false),
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
