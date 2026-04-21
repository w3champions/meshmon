import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster, toast } from "sonner";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { Campaign, CampaignState } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";

// ---------------------------------------------------------------------------
// Module mocks
// ---------------------------------------------------------------------------

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return { ...actual, useEvaluateCampaign: vi.fn(), useTriggerDetail: vi.fn() };
});

import { useEvaluateCampaign, useTriggerDetail } from "@/api/hooks/evaluation";
import { OverflowMenu } from "@/components/campaigns/results/OverflowMenu";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

function makeCampaign(overrides: Partial<Campaign> & { state: CampaignState }): Campaign {
  return {
    id: overrides.id ?? CAMPAIGN_ID,
    title: "Campaign alpha",
    notes: "",
    state: overrides.state,
    protocol: "icmp",
    evaluation_mode: "optimization",
    force_measurement: false,
    loss_threshold_pct: 2,
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
      ["succeeded", 10],
      ["reused", 5],
      ["pending", 2],
    ],
  };
}

function makeEvaluation(): Evaluation {
  return {
    campaign_id: CAMPAIGN_ID,
    evaluated_at: "2026-04-21T10:00:00Z",
    loss_threshold_pct: 2,
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
          display_name: "cand-one",
          city: "Berlin",
          country_code: "DE",
          asn: 123,
          network_operator: "NetA",
          is_mesh_member: false,
          pairs_improved: 2,
          pairs_total_considered: 3,
          avg_improvement_ms: 30,
          avg_loss_pct: 0.1,
          composite_score: 20,
          pair_details: [
            {
              source_agent_id: "agent-a",
              destination_agent_id: "agent-b",
              destination_ip: "10.0.0.1",
              direct_rtt_ms: 50,
              direct_stddev_ms: 2,
              direct_loss_pct: 0.1,
              transit_rtt_ms: 20,
              transit_stddev_ms: 1,
              transit_loss_pct: 0.05,
              improvement_ms: 30,
              qualifies: true,
              mtr_measurement_id_ax: null,
              mtr_measurement_id_xb: null,
            },
            {
              source_agent_id: "agent-b",
              destination_agent_id: "agent-c",
              destination_ip: "10.0.0.1",
              direct_rtt_ms: 50,
              direct_stddev_ms: 2,
              direct_loss_pct: 0.1,
              transit_rtt_ms: 20,
              transit_stddev_ms: 1,
              transit_loss_pct: 0.05,
              improvement_ms: 30,
              qualifies: true,
              mtr_measurement_id_ax: null,
              mtr_measurement_id_xb: null,
            },
            {
              source_agent_id: "agent-a",
              destination_agent_id: "agent-d",
              destination_ip: "10.0.0.1",
              direct_rtt_ms: 50,
              direct_stddev_ms: 2,
              direct_loss_pct: 0.1,
              transit_rtt_ms: 20,
              transit_stddev_ms: 1,
              transit_loss_pct: 0.05,
              improvement_ms: -5,
              qualifies: false,
              mtr_measurement_id_ax: null,
              mtr_measurement_id_xb: null,
            },
          ],
        },
      ],
      unqualified_reasons: {},
    },
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

const evaluateStub = makeMutationStub();
const triggerDetailStub = makeMutationStub();

function setupMocks() {
  vi.mocked(useEvaluateCampaign).mockReturnValue(
    evaluateStub as unknown as ReturnType<typeof useEvaluateCampaign>,
  );
  vi.mocked(useTriggerDetail).mockReturnValue(
    triggerDetailStub as unknown as ReturnType<typeof useTriggerDetail>,
  );
}

function renderMenu(campaign: Campaign, evaluation: Evaluation | null = null) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <OverflowMenu campaign={campaign} evaluation={evaluation} />
      <Toaster />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  evaluateStub.mutate.mockReset();
  triggerDetailStub.mutate.mockReset();
  setupMocks();
});

afterEach(() => {
  cleanup();
  toast.dismiss();
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("OverflowMenu — gating", () => {
  test("good-candidates item is disabled off evaluated state", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "completed" }));

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await waitFor(() => expect(screen.getByTestId("overflow-detail-good")).toBeInTheDocument());
    expect(screen.getByTestId("overflow-detail-good")).toHaveAttribute("aria-disabled", "true");
  });

  test("good-candidates item is enabled when campaign is evaluated", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await waitFor(() => expect(screen.getByTestId("overflow-detail-good")).toBeInTheDocument());
    expect(screen.getByTestId("overflow-detail-good")).toHaveAttribute("aria-disabled", "false");
  });
});

describe("OverflowMenu — re-evaluate", () => {
  test("Re-evaluate fires useEvaluateCampaign directly without the cost dialog", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await user.click(screen.getByTestId("overflow-re-evaluate"));

    expect(evaluateStub.mutate).toHaveBeenCalledTimes(1);
    expect(evaluateStub.mutate.mock.calls[0][0]).toBe(CAMPAIGN_ID);
    // No dialog opened
    expect(screen.queryByTestId("detail-cost-preview")).not.toBeInTheDocument();
  });

  test("no_baseline_pairs on re-evaluate surfaces a dedicated toast", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    const err = new Error("failed", { cause: { error: "no_baseline_pairs" } });
    evaluateStub.mutate.mockImplementation(
      (_id: unknown, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await user.click(screen.getByTestId("overflow-re-evaluate"));

    await waitFor(() => {
      expect(screen.getByText(/no baseline measurements/i)).toBeInTheDocument();
    });
  });
});

describe("OverflowMenu — Detail: all dialog", () => {
  test("opens the cost-preview dialog with scope=all and a 2× settled-pairs estimate", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await user.click(screen.getByTestId("overflow-detail-all"));

    await waitFor(() => {
      expect(screen.getByTestId("detail-cost-preview")).toBeInTheDocument();
    });
    // settled = 10 succeeded + 5 reused = 15; enqueue = 30.
    expect(screen.getByTestId("cost-preview-pairs")).toHaveTextContent("30");
  });

  test("confirm dispatches a scope=all detail request", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await user.click(screen.getByTestId("overflow-detail-all"));

    await waitFor(() => {
      expect(screen.getByTestId("cost-preview-confirm")).toBeInTheDocument();
    });
    await user.click(screen.getByTestId("cost-preview-confirm"));

    expect(triggerDetailStub.mutate).toHaveBeenCalledTimes(1);
    const [vars] = triggerDetailStub.mutate.mock.calls[0];
    expect(vars).toEqual({ id: CAMPAIGN_ID, body: { scope: "all" } });
  });

  test("no_pairs_selected error surfaces an informational toast", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    const err = new Error("failed", { cause: { error: "no_pairs_selected" } });
    triggerDetailStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await user.click(screen.getByTestId("overflow-detail-all"));
    await user.click(screen.getByTestId("cost-preview-confirm"));

    await waitFor(() => {
      expect(screen.getByText(/nothing left to re-measure/i)).toBeInTheDocument();
    });
  });
});

describe("OverflowMenu — Detail: good candidates dialog", () => {
  test("good-candidates preview counts qualifying triples × 4 with de-dup", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await user.click(screen.getByTestId("overflow-detail-good"));

    await waitFor(() => {
      expect(screen.getByTestId("detail-cost-preview")).toBeInTheDocument();
    });
    // Two qualifying pair_details × 4 = 8 (third is qualifies=false).
    expect(screen.getByTestId("cost-preview-pairs")).toHaveTextContent("8");
  });

  test("confirm dispatches a scope=good_candidates detail request", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await user.click(screen.getByTestId("overflow-detail-good"));
    await user.click(screen.getByTestId("cost-preview-confirm"));

    expect(triggerDetailStub.mutate).toHaveBeenCalledTimes(1);
    const [vars] = triggerDetailStub.mutate.mock.calls[0];
    expect(vars).toEqual({ id: CAMPAIGN_ID, body: { scope: "good_candidates" } });
  });

  test("cancel closes the dialog without firing the mutation", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await user.click(screen.getByTestId("overflow-detail-good"));

    await waitFor(() => {
      expect(screen.getByTestId("detail-cost-preview")).toBeInTheDocument();
    });
    await user.click(screen.getByRole("button", { name: /cancel/i }));

    await waitFor(() => {
      expect(screen.queryByTestId("detail-cost-preview")).not.toBeInTheDocument();
    });
    expect(triggerDetailStub.mutate).not.toHaveBeenCalled();
  });

  test("illegal_state_transition error surfaces a dedicated toast", async () => {
    const user = userEvent.setup();
    renderMenu(makeCampaign({ state: "evaluated" }), makeEvaluation());

    const err = new Error("failed", { cause: { error: "illegal_state_transition" } });
    triggerDetailStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByTestId("candidates-overflow-trigger"));
    await user.click(screen.getByTestId("overflow-detail-good"));
    await user.click(screen.getByTestId("cost-preview-confirm"));

    await waitFor(() => {
      expect(screen.getByText(/campaign is still running/i)).toBeInTheDocument();
    });
  });
});
