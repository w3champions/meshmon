import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster, toast } from "sonner";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { Campaign, CampaignState } from "@/api/hooks/campaigns";
import { SettingsTab } from "@/components/campaigns/results/SettingsTab";

// ---------------------------------------------------------------------------
// Module mocks — register BEFORE importing the component so the real hooks
// never resolve. The component under test pulls three hooks:
//   - `useEvaluation` (GET /evaluation)
//   - `usePatchCampaign` (PATCH /campaigns/{id})
//   - `useEvaluateCampaign` (POST /evaluate)
// Stub each so the test can drive success / error branches deterministically.
// ---------------------------------------------------------------------------

vi.mock("@/api/hooks/campaigns", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/campaigns")>("@/api/hooks/campaigns");
  return { ...actual, usePatchCampaign: vi.fn() };
});

vi.mock("@/api/hooks/evaluation", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/evaluation")>("@/api/hooks/evaluation");
  return { ...actual, useEvaluation: vi.fn(), useEvaluateCampaign: vi.fn() };
});

import { usePatchCampaign } from "@/api/hooks/campaigns";
import { useEvaluateCampaign, useEvaluation } from "@/api/hooks/evaluation";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";

function makeCampaign(overrides: Partial<Campaign> & { state: CampaignState }): Campaign {
  return {
    id: overrides.id ?? CAMPAIGN_ID,
    title: overrides.title ?? "Campaign beta",
    notes: overrides.notes ?? "notes",
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
    max_transit_rtt_ms: overrides.max_transit_rtt_ms ?? null,
    max_transit_stddev_ms: overrides.max_transit_stddev_ms ?? null,
    min_improvement_ms: overrides.min_improvement_ms ?? null,
    min_improvement_ratio: overrides.min_improvement_ratio ?? null,
    useful_latency_ms: overrides.useful_latency_ms ?? null,
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

interface MutationStub {
  mutate: ReturnType<typeof vi.fn>;
  mutateAsync: ReturnType<typeof vi.fn>;
  isPending: boolean;
  reset: ReturnType<typeof vi.fn>;
}

function makeMutationStub(): MutationStub {
  return { mutate: vi.fn(), mutateAsync: vi.fn(), isPending: false, reset: vi.fn() };
}

const patchStub = makeMutationStub();
const evaluateStub = makeMutationStub();

function setupMocks(): void {
  // Default: no prior evaluation. Seed the form from the campaign row.
  vi.mocked(useEvaluation).mockReturnValue({
    data: null,
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useEvaluation>);

  vi.mocked(usePatchCampaign).mockReturnValue(
    patchStub as unknown as ReturnType<typeof usePatchCampaign>,
  );
  vi.mocked(useEvaluateCampaign).mockReturnValue(
    evaluateStub as unknown as ReturnType<typeof useEvaluateCampaign>,
  );
}

function renderTab(campaign: Campaign) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <SettingsTab campaign={campaign} />
      <Toaster />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  patchStub.mutate.mockReset();
  evaluateStub.mutate.mockReset();
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

describe("SettingsTab — form seeding", () => {
  test("seeds the inputs from the campaign when no evaluation row exists", async () => {
    renderTab(
      makeCampaign({
        state: "completed",
        // Wire ratio 0.075 renders as "7.5" in the percent-facing input.
        loss_threshold_ratio: 0.075,
        stddev_weight: 1.75,
        evaluation_mode: "diversity",
      }),
    );

    const lossInput = screen.getByLabelText(/loss threshold/i) as HTMLInputElement;
    const stddevInput = screen.getByLabelText(/stddev weight/i) as HTMLInputElement;
    expect(lossInput.value).toBe("7.5");
    expect(stddevInput.value).toBe("1.75");
    // The Diversity toggle should be pressed.
    expect(screen.getByRole("radio", { name: /diversity/i })).toHaveAttribute("data-state", "on");
  });

  test("prefers the evaluation row snapshot when present", async () => {
    vi.mocked(useEvaluation).mockReturnValue({
      data: {
        id: "eval-1",
        campaign_id: CAMPAIGN_ID,
        evaluated_at: "2026-04-10T12:00:00Z",
        // Wire ratio 0.0325 renders as "3.25" in the percent-facing input.
        loss_threshold_ratio: 0.0325,
        stddev_weight: 0.5,
        evaluation_mode: "diversity",
        baseline_pair_count: 4,
        candidates_total: 2,
        candidates_good: 1,
        avg_improvement_ms: 12,
        results: { candidates: [], unqualified_reasons: {} },
      },
      isLoading: false,
      isError: false,
    } as unknown as ReturnType<typeof useEvaluation>);

    renderTab(
      makeCampaign({
        state: "evaluated",
        // Wire ratio 0.09 renders as "9" in the percent-facing input —
        // but the evaluation-row snapshot takes precedence and shows "3.25".
        loss_threshold_ratio: 0.09,
        stddev_weight: 9,
        evaluation_mode: "optimization",
      }),
    );

    const lossInput = screen.getByLabelText(/loss threshold/i) as HTMLInputElement;
    expect(lossInput.value).toBe("3.25");
    // Evaluated footer references the evaluation-row timestamp.
    expect(screen.getByText(/last evaluated/i)).toBeInTheDocument();
  });
});

describe("SettingsTab — eligibility gate", () => {
  test("disables the submit button on running campaigns", () => {
    renderTab(makeCampaign({ state: "running" }));

    const submit = screen.getByRole("button", { name: /re-evaluate/i });
    expect(submit).toBeDisabled();
    expect(screen.getByLabelText(/loss threshold/i)).toBeDisabled();
  });

  test("enables the submit button on completed campaigns", () => {
    renderTab(makeCampaign({ state: "completed" }));
    expect(screen.getByRole("button", { name: /re-evaluate/i })).not.toBeDisabled();
  });

  test("enables the submit button on evaluated campaigns", () => {
    renderTab(makeCampaign({ state: "evaluated" }));
    expect(screen.getByRole("button", { name: /re-evaluate/i })).not.toBeDisabled();
  });

  test("draft state disables re-evaluate button", () => {
    renderTab(makeCampaign({ state: "draft" }));

    const submit = screen.getByRole("button", { name: /re-evaluate/i });
    expect(submit).toBeDisabled();
    expect(screen.getByLabelText(/loss threshold/i)).toBeDisabled();
  });

  test("stopped state disables re-evaluate button", () => {
    renderTab(makeCampaign({ state: "stopped" }));

    const submit = screen.getByRole("button", { name: /re-evaluate/i });
    expect(submit).toBeDisabled();
    expect(screen.getByLabelText(/loss threshold/i)).toBeDisabled();
  });
});

describe("SettingsTab — submit flow", () => {
  test("submitting with edited knobs patches the campaign then triggers evaluate", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    // Tweak the threshold. `fireEvent.change` replaces the value atomically;
    // user-event's `clear` + `type` leaves residual characters on number
    // inputs under jsdom, so we go the direct route here. The component
    // subscribes to `onChange` so the form state updates identically.
    const loss = screen.getByLabelText(/loss threshold/i) as HTMLInputElement;
    fireEvent.change(loss, { target: { value: "4.5" } });

    // Switch to diversity.
    await user.click(screen.getByRole("radio", { name: /diversity/i }));

    // Wire patch success so the mutation calls through to evaluate.
    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onSuccess?: (result: unknown) => void }) => {
        opts?.onSuccess?.({});
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    expect(patchStub.mutate).toHaveBeenCalledTimes(1);
    const [patchVars] = patchStub.mutate.mock.calls[0];
    expect(patchVars).toEqual({
      id: CAMPAIGN_ID,
      body: {
        // Form input "4.5" percent → 0.045 ratio on the wire.
        loss_threshold_ratio: 0.045,
        stddev_weight: 1,
        evaluation_mode: "diversity",
        // Guardrails default to `null` on a campaign that never had
        // them set, and ride along on every PATCH. The backend's
        // COALESCE semantics preserve the column when the wire value
        // is `null`.
        max_transit_rtt_ms: null,
        max_transit_stddev_ms: null,
        min_improvement_ms: null,
        min_improvement_ratio: null,
        // New edge_candidate knobs always ride on the PATCH body.
        max_hops: 2,
        vm_lookback_minutes: 15,
      },
    });

    // Patch → evaluate chain.
    expect(evaluateStub.mutate).toHaveBeenCalledTimes(1);
    const [evaluateVars] = evaluateStub.mutate.mock.calls[0];
    expect(evaluateVars).toBe(CAMPAIGN_ID);

    // Ordering contract: PATCH must run BEFORE POST /evaluate, because
    // /evaluate has no request body and reads the knobs off the campaign
    // row. Compare the vitest-tracked invocation order numbers so a future
    // refactor can't accidentally flip the call sequence without failing.
    const patchOrder = patchStub.mutate.mock.invocationCallOrder[0];
    const evaluateOrder = evaluateStub.mutate.mock.invocationCallOrder[0];
    expect(patchOrder).toBeLessThan(evaluateOrder);
  });

  test("surfaces a no_baseline_pairs toast when the evaluator rejects the request", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    // Patch succeeds, evaluate fails with the backend's 422 envelope.
    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onSuccess?: (result: unknown) => void }) => {
        opts?.onSuccess?.({});
      },
    );
    const noBaselineErr = new Error("failed", {
      cause: { error: "no_baseline_pairs" },
    });
    evaluateStub.mutate.mockImplementation(
      (_id: string, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(noBaselineErr);
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    await waitFor(() => {
      // Sonner renders into the Toaster; the operator copy lands there.
      expect(
        screen.getByText(/no agent-to-agent baseline measurements exist/i),
      ).toBeInTheDocument();
    });
    // The form is intact — inputs kept their values.
    expect(screen.getByLabelText(/loss threshold/i)).toBeInTheDocument();
  });

  test("surfaces a vm_upstream toast with the detail reason when VM is unreachable", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onSuccess?: (result: unknown) => void }) => {
        opts?.onSuccess?.({});
      },
    );
    const err = new Error("failed", {
      cause: { error: "vm_upstream", detail: "connect: connection refused" },
    });
    evaluateStub.mutate.mockImplementation(
      (_id: string, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    await waitFor(() => {
      expect(
        screen.getByText(
          /victoriametrics couldn't be reached for baseline data \(connect: connection refused\)/i,
        ),
      ).toBeInTheDocument();
    });
  });

  test("surfaces an illegal_state_transition toast when patch is raced by the backend", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    const err = new Error("failed", { cause: { error: "illegal_state_transition" } });
    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    await waitFor(() => {
      expect(screen.getByText(/refresh and retry/i)).toBeInTheDocument();
    });
    // Evaluate is never called when patch fails.
    expect(evaluateStub.mutate).not.toHaveBeenCalled();
  });

  test("happy path — both mutations succeed, no error toast fires", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    // Wire patch to resolve synchronously with a plausible payload, then
    // evaluate to succeed with its own Evaluation DTO. Together this
    // exercises the end-to-end re-evaluate flow.
    patchStub.mutate.mockImplementation(
      (
        _vars: unknown,
        opts?: {
          onSuccess?: (result: unknown) => void;
          onError?: (err: Error) => void;
        },
      ) => {
        opts?.onSuccess?.({
          id: CAMPAIGN_ID,
          loss_threshold_ratio: 0.045,
          stddev_weight: 1,
          evaluation_mode: "diversity",
        });
      },
    );
    evaluateStub.mutate.mockImplementation(
      (
        _id: string,
        opts?: {
          onSuccess?: (result: unknown) => void;
          onError?: (err: Error) => void;
        },
      ) => {
        opts?.onSuccess?.({
          id: "eval-2",
          campaign_id: CAMPAIGN_ID,
          evaluated_at: "2026-04-21T10:00:00Z",
          loss_threshold_ratio: 0.045,
          stddev_weight: 1,
          evaluation_mode: "diversity",
          baseline_pair_count: 3,
          candidates_total: 5,
          candidates_good: 2,
          avg_improvement_ms: 14,
          results: { candidates: [], unqualified_reasons: {} },
        });
      },
    );

    // Adjust the form to match the wired mutation payloads (makes the
    // assertion below meaningful — the inputs reflect what got submitted
    // and echoed back).
    const loss = screen.getByLabelText(/loss threshold/i) as HTMLInputElement;
    fireEvent.change(loss, { target: { value: "4.5" } });
    await user.click(screen.getByRole("radio", { name: /diversity/i }));

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    // Both mutations ran.
    expect(patchStub.mutate).toHaveBeenCalledTimes(1);
    expect(evaluateStub.mutate).toHaveBeenCalledTimes(1);

    // No error copy anywhere — the failure-path toasts never fire.
    expect(screen.queryByText(/failed/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/refresh and retry/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/no baseline measurements/i)).not.toBeInTheDocument();

    // Form values reflect the submitted knobs.
    expect((screen.getByLabelText(/loss threshold/i) as HTMLInputElement).value).toBe("4.5");
    expect(screen.getByRole("radio", { name: /diversity/i })).toHaveAttribute("data-state", "on");
  });
});

// ---------------------------------------------------------------------------
// Guardrail knobs — round-trip, clearing, and footgun warning
// ---------------------------------------------------------------------------

describe("SettingsTab — guardrail knobs", () => {
  test("seeds guardrail inputs from the campaign when no evaluation row exists", () => {
    renderTab(
      makeCampaign({
        state: "completed",
        max_transit_rtt_ms: 250,
        max_transit_stddev_ms: 40,
        min_improvement_ms: 5,
        min_improvement_ratio: 0.1,
      }),
    );

    expect((screen.getByLabelText(/max transit rtt \(ms\)/i) as HTMLInputElement).value).toBe(
      "250",
    );
    expect(
      (screen.getByLabelText(/max transit rtt stddev \(ms\)/i) as HTMLInputElement).value,
    ).toBe("40");
    expect((screen.getByLabelText(/min improvement \(ms\)/i) as HTMLInputElement).value).toBe("5");
    expect((screen.getByLabelText(/min improvement ratio/i) as HTMLInputElement).value).toBe("0.1");
  });

  test("prefers the evaluation snapshot over the campaign for guardrail inputs", () => {
    vi.mocked(useEvaluation).mockReturnValue({
      data: {
        id: "eval-g1",
        campaign_id: CAMPAIGN_ID,
        evaluated_at: "2026-04-12T12:00:00Z",
        loss_threshold_ratio: 0.02,
        stddev_weight: 1,
        evaluation_mode: "optimization",
        baseline_pair_count: 4,
        candidates_total: 2,
        candidates_good: 1,
        avg_improvement_ms: 12,
        // Snapshot wins over campaign-row values below.
        max_transit_rtt_ms: 150,
        max_transit_stddev_ms: null,
        min_improvement_ms: null,
        min_improvement_ratio: 0.25,
        results: { candidates: [], unqualified_reasons: {} },
      },
      isLoading: false,
      isError: false,
    } as unknown as ReturnType<typeof useEvaluation>);

    renderTab(
      makeCampaign({
        state: "evaluated",
        max_transit_rtt_ms: 999,
        min_improvement_ratio: 0.99,
      }),
    );

    expect((screen.getByLabelText(/max transit rtt \(ms\)/i) as HTMLInputElement).value).toBe(
      "150",
    );
    expect((screen.getByLabelText(/min improvement ratio/i) as HTMLInputElement).value).toBe(
      "0.25",
    );
    // Empty snapshot fields render as empty inputs.
    expect((screen.getByLabelText(/min improvement \(ms\)/i) as HTMLInputElement).value).toBe("");
  });

  test("submitting with a guardrail value carries it on the PATCH body", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    fireEvent.change(screen.getByLabelText(/max transit rtt \(ms\)/i), {
      target: { value: "200" },
    });
    fireEvent.change(screen.getByLabelText(/min improvement \(ms\)/i), {
      target: { value: "5" },
    });

    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onSuccess?: (result: unknown) => void }) => {
        opts?.onSuccess?.({});
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    expect(patchStub.mutate).toHaveBeenCalledTimes(1);
    const [patchVars] = patchStub.mutate.mock.calls[0];
    expect(patchVars).toEqual({
      id: CAMPAIGN_ID,
      body: {
        loss_threshold_ratio: 0.02,
        stddev_weight: 1,
        evaluation_mode: "optimization",
        max_transit_rtt_ms: 200,
        max_transit_stddev_ms: null,
        min_improvement_ms: 5,
        min_improvement_ratio: null,
        max_hops: 2,
        vm_lookback_minutes: 15,
      },
    });
  });

  test("accepts a negative min_improvement_ms and round-trips it on submit", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    // Negative improvement floor is accepted by spec — operators can keep
    // "near-baseline" rows that are fractionally slower but more stable.
    fireEvent.change(screen.getByLabelText(/min improvement \(ms\)/i), {
      target: { value: "-10" },
    });

    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onSuccess?: (result: unknown) => void }) => {
        opts?.onSuccess?.({});
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    const [patchVars] = patchStub.mutate.mock.calls[0];
    expect(
      (patchVars as { body: { min_improvement_ms: number | null } }).body.min_improvement_ms,
    ).toBe(-10);
  });

  test("clearing a guardrail input resets the local form state to null", async () => {
    const user = userEvent.setup();
    renderTab(
      makeCampaign({
        state: "completed",
        max_transit_rtt_ms: 250,
      }),
    );

    const input = screen.getByLabelText(/max transit rtt \(ms\)/i) as HTMLInputElement;
    expect(input.value).toBe("250");

    // Empty input → null in form state. The PATCH body carries the
    // local form's `null`; the backend's COALESCE semantics preserve
    // the existing column value rather than nulling it. The
    // assertion below verifies the wire shape; the limitation is
    // documented in the form copy.
    fireEvent.change(input, { target: { value: "" } });
    expect(input.value).toBe("");

    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onSuccess?: (result: unknown) => void }) => {
        opts?.onSuccess?.({});
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    const [patchVars] = patchStub.mutate.mock.calls[0];
    expect(
      (patchVars as { body: { max_transit_rtt_ms: number | null } }).body.max_transit_rtt_ms,
    ).toBeNull();
  });

  test("clamps guardrail input when the operator types out-of-range", () => {
    renderTab(makeCampaign({ state: "completed" }));

    const input = screen.getByLabelText(/max transit rtt \(ms\)/i) as HTMLInputElement;

    // Above max (10000) clamps to max.
    fireEvent.change(input, { target: { value: "999999" } });
    expect(input.value).toBe("10000");

    // Below min (1) clamps to min.
    fireEvent.change(input, { target: { value: "-5" } });
    expect(input.value).toBe("1");
  });
});

// ---------------------------------------------------------------------------
// Operator-footgun warning — fires when guardrails dropped every candidate
// ---------------------------------------------------------------------------

function evaluationStub(overrides: {
  candidates_total: number;
  candidates_good?: number;
  max_transit_rtt_ms?: number | null;
  max_transit_stddev_ms?: number | null;
  min_improvement_ms?: number | null;
  min_improvement_ratio?: number | null;
}) {
  return {
    data: {
      id: "eval-warn",
      campaign_id: CAMPAIGN_ID,
      evaluated_at: "2026-04-15T12:00:00Z",
      loss_threshold_ratio: 0.02,
      stddev_weight: 1,
      evaluation_mode: "optimization",
      baseline_pair_count: 4,
      candidates_total: overrides.candidates_total,
      candidates_good: overrides.candidates_good ?? 0,
      avg_improvement_ms: 0,
      max_transit_rtt_ms: overrides.max_transit_rtt_ms ?? null,
      max_transit_stddev_ms: overrides.max_transit_stddev_ms ?? null,
      min_improvement_ms: overrides.min_improvement_ms ?? null,
      min_improvement_ratio: overrides.min_improvement_ratio ?? null,
      results: { candidates: [], unqualified_reasons: {} },
    },
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useEvaluation>;
}

describe("SettingsTab — guardrail footgun warning", () => {
  test("renders when candidates_total is 0 AND a guardrail is set", () => {
    vi.mocked(useEvaluation).mockReturnValue(
      evaluationStub({ candidates_total: 0, max_transit_rtt_ms: 50 }),
    );
    renderTab(makeCampaign({ state: "evaluated" }));

    expect(screen.getByText(/dropped every candidate/i)).toBeInTheDocument();
  });

  test("does NOT render when candidates_total is 0 but no guardrail is set", () => {
    vi.mocked(useEvaluation).mockReturnValue(evaluationStub({ candidates_total: 0 }));
    renderTab(makeCampaign({ state: "evaluated" }));

    expect(screen.queryByText(/dropped every candidate/i)).not.toBeInTheDocument();
  });

  test("does NOT render when candidates_total > 0", () => {
    vi.mocked(useEvaluation).mockReturnValue(
      evaluationStub({
        candidates_total: 3,
        candidates_good: 2,
        max_transit_rtt_ms: 50,
      }),
    );
    renderTab(makeCampaign({ state: "evaluated" }));

    expect(screen.queryByText(/dropped every candidate/i)).not.toBeInTheDocument();
  });

  test("does NOT render when no evaluation row exists yet", () => {
    // Default mock: no evaluation snapshot. The warning depends on the
    // snapshot, so it must not render in this state.
    renderTab(makeCampaign({ state: "completed", max_transit_rtt_ms: 50 }));

    expect(screen.queryByText(/dropped every candidate/i)).not.toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// R1 — edge_candidate mode selector + mode-aware knobs
// ---------------------------------------------------------------------------

describe("SettingsTab — R1: edge_candidate mode", () => {
  test("mode selector has three items: Diversity, Optimization, Edge candidate", () => {
    renderTab(makeCampaign({ state: "completed" }));

    expect(screen.getByRole("radio", { name: /diversity/i })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /optimization/i })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /edge candidate/i })).toBeInTheDocument();
  });

  test("mode selector is placed before the loss-threshold inputs", () => {
    renderTab(makeCampaign({ state: "completed" }));

    const form = screen.getByRole("form", { name: /re-evaluate/i });
    const modeGroup = form.querySelector("[aria-labelledby='settings-evaluation-mode-label']");
    const lossInput = screen.getByLabelText(/loss threshold/i);

    expect(modeGroup).not.toBeNull();
    // compareDocumentPosition: if modeGroup precedes lossInput, the flag includes DOCUMENT_POSITION_FOLLOWING (4)
    const position = modeGroup!.compareDocumentPosition(lossInput);
    expect(position & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  test("clicking Edge candidate switches mode and shows edge hint", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    await user.click(screen.getByRole("radio", { name: /edge candidate/i }));

    expect(screen.getByRole("radio", { name: /edge candidate/i })).toHaveAttribute(
      "data-state",
      "on",
    );
    expect(screen.getByText(/direct \+ transitive/i)).toBeInTheDocument();
  });

  test("edge_candidate mode shows useful_latency_ms and vm_lookback_minutes inputs", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    await user.click(screen.getByRole("radio", { name: /edge candidate/i }));

    expect(screen.getByLabelText(/useful latency/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/lookback window/i)).toBeInTheDocument();
  });

  test("diversity mode hides useful_latency_ms and vm_lookback_minutes inputs", () => {
    renderTab(makeCampaign({ state: "completed", evaluation_mode: "diversity" }));

    expect(screen.queryByLabelText(/useful latency/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/lookback window/i)).not.toBeInTheDocument();
  });

  test("optimization mode hides useful_latency_ms and vm_lookback_minutes inputs", () => {
    renderTab(makeCampaign({ state: "completed", evaluation_mode: "optimization" }));

    expect(screen.queryByLabelText(/useful latency/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/lookback window/i)).not.toBeInTheDocument();
  });

  test("edge_candidate mode hides min_improvement_ms and min_improvement_ratio", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    await user.click(screen.getByRole("radio", { name: /edge candidate/i }));

    expect(screen.queryByLabelText(/min improvement \(ms\)/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/min improvement ratio/i)).not.toBeInTheDocument();
  });

  test("diversity mode shows min_improvement_ms and min_improvement_ratio", () => {
    renderTab(makeCampaign({ state: "completed", evaluation_mode: "diversity" }));

    expect(screen.getByLabelText(/min improvement \(ms\)/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/min improvement ratio/i)).toBeInTheDocument();
  });

  test("max_hops has 'Direct only' option only in edge_candidate mode", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    // optimization: no "Direct only"
    expect(screen.queryByRole("radio", { name: /direct only/i })).not.toBeInTheDocument();

    await user.click(screen.getByRole("radio", { name: /edge candidate/i }));

    // edge_candidate: "Direct only" appears
    expect(screen.getByRole("radio", { name: /direct only/i })).toBeInTheDocument();
  });

  test("useful_latency_ms required validation: Submit disabled when null in edge_candidate mode", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    await user.click(screen.getByRole("radio", { name: /edge candidate/i }));

    // useful_latency_ms starts as null → button should be disabled
    const submit = screen.getByRole("button", { name: /re-evaluate/i });
    expect(submit).toBeDisabled();
    expect(screen.getByText(/required for edge candidate/i)).toBeInTheDocument();
  });

  test("useful_latency_ms set: Submit enabled in edge_candidate mode", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    await user.click(screen.getByRole("radio", { name: /edge candidate/i }));

    fireEvent.change(screen.getByLabelText(/useful latency/i), { target: { value: "80" } });

    const submit = screen.getByRole("button", { name: /re-evaluate/i });
    expect(submit).not.toBeDisabled();
  });

  test("PATCH body includes max_hops, vm_lookback_minutes, and useful_latency_ms in edge_candidate mode", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    await user.click(screen.getByRole("radio", { name: /edge candidate/i }));
    fireEvent.change(screen.getByLabelText(/useful latency/i), { target: { value: "80" } });

    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onSuccess?: (result: unknown) => void }) => {
        opts?.onSuccess?.({});
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    expect(patchStub.mutate).toHaveBeenCalledTimes(1);
    const [patchVars] = patchStub.mutate.mock.calls[0];
    const body = (patchVars as { body: Record<string, unknown> }).body;
    expect(body.evaluation_mode).toBe("edge_candidate");
    expect(body.max_hops).toBe(2);
    expect(body.vm_lookback_minutes).toBe(15);
    expect(body.useful_latency_ms).toBe(80);
  });

  test("PATCH body omits useful_latency_ms in diversity mode", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed", evaluation_mode: "diversity" }));

    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onSuccess?: (result: unknown) => void }) => {
        opts?.onSuccess?.({});
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    const [patchVars] = patchStub.mutate.mock.calls[0];
    const body = (patchVars as { body: Record<string, unknown> }).body;
    expect("useful_latency_ms" in body).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// R2 — "legacy" badge for NULL-snapshot evaluations
// ---------------------------------------------------------------------------

describe("SettingsTab — R2: legacy badge", () => {
  test("renders legacy badge when snapshot.max_hops is null", () => {
    vi.mocked(useEvaluation).mockReturnValue({
      data: {
        id: "eval-legacy",
        campaign_id: CAMPAIGN_ID,
        evaluated_at: "2026-04-10T12:00:00Z",
        loss_threshold_ratio: 0.02,
        stddev_weight: 1,
        evaluation_mode: "optimization",
        baseline_pair_count: 4,
        candidates_total: 2,
        candidates_good: 1,
        avg_improvement_ms: 12,
        max_hops: null,
        vm_lookback_minutes: null,
        results: { candidates: [], unqualified_reasons: {} },
      },
      isLoading: false,
      isError: false,
    } as unknown as ReturnType<typeof useEvaluation>);

    renderTab(makeCampaign({ state: "evaluated" }));

    expect(screen.getByText(/legacy/i)).toBeInTheDocument();
  });

  test("does NOT render legacy badge when snapshot.max_hops is non-null", () => {
    vi.mocked(useEvaluation).mockReturnValue({
      data: {
        id: "eval-modern",
        campaign_id: CAMPAIGN_ID,
        evaluated_at: "2026-04-10T12:00:00Z",
        loss_threshold_ratio: 0.02,
        stddev_weight: 1,
        evaluation_mode: "optimization",
        baseline_pair_count: 4,
        candidates_total: 2,
        candidates_good: 1,
        avg_improvement_ms: 12,
        max_hops: 2,
        vm_lookback_minutes: 15,
        results: { candidates: [], unqualified_reasons: {} },
      },
      isLoading: false,
      isError: false,
    } as unknown as ReturnType<typeof useEvaluation>);

    renderTab(makeCampaign({ state: "evaluated" }));

    expect(screen.queryByText(/legacy/i)).not.toBeInTheDocument();
  });

  test("does NOT render legacy badge when there is no evaluation row", () => {
    renderTab(makeCampaign({ state: "completed" }));

    expect(screen.queryByText(/legacy/i)).not.toBeInTheDocument();
  });

  test("renders legacy badge when snapshot.max_hops is undefined", () => {
    vi.mocked(useEvaluation).mockReturnValue({
      data: {
        id: "eval-legacy-undef",
        campaign_id: CAMPAIGN_ID,
        evaluated_at: "2026-04-10T12:00:00Z",
        loss_threshold_ratio: 0.02,
        stddev_weight: 1,
        evaluation_mode: "optimization",
        baseline_pair_count: 4,
        candidates_total: 2,
        candidates_good: 1,
        avg_improvement_ms: 12,
        // max_hops intentionally absent (undefined) — pre-T56 evaluations
        vm_lookback_minutes: null,
        results: { candidates: [], unqualified_reasons: {} },
      } as any,
      isLoading: false,
      isError: false,
    } as unknown as ReturnType<typeof useEvaluation>);

    renderTab(makeCampaign({ state: "evaluated" }));

    expect(screen.getByText(/legacy/i)).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// R3 — single-source-agent banner for edge_candidate
// ---------------------------------------------------------------------------

describe("SettingsTab — R3: single-source-agent banner", () => {
  function makeEvalEdgeCandidate() {
    return {
      data: {
        id: "eval-edge",
        campaign_id: CAMPAIGN_ID,
        evaluated_at: "2026-04-10T12:00:00Z",
        loss_threshold_ratio: 0.02,
        stddev_weight: 1,
        evaluation_mode: "edge_candidate",
        baseline_pair_count: 4,
        candidates_total: 2,
        candidates_good: 1,
        avg_improvement_ms: 12,
        max_hops: 2,
        vm_lookback_minutes: 15,
        results: { candidates: [], unqualified_reasons: {} },
      },
      isLoading: false,
      isError: false,
    } as unknown as ReturnType<typeof useEvaluation>;
  }

  test("renders banner when evaluation_mode=edge_candidate AND single source agent", () => {
    vi.mocked(useEvaluation).mockReturnValue(makeEvalEdgeCandidate());

    const campaign = {
      ...makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      source_agent_ids: ["agent-1"],
    } as unknown as Campaign;

    renderTab(campaign);

    expect(screen.getByRole("status")).toBeInTheDocument();
    expect(screen.getByText(/only one source agent/i)).toBeInTheDocument();
  });

  test("does NOT render banner when evaluation_mode=edge_candidate AND multiple source agents", () => {
    vi.mocked(useEvaluation).mockReturnValue(makeEvalEdgeCandidate());

    const campaign = {
      ...makeCampaign({ state: "evaluated", evaluation_mode: "edge_candidate" }),
      source_agent_ids: ["agent-1", "agent-2"],
    } as unknown as Campaign;

    renderTab(campaign);

    expect(screen.queryByText(/only one source agent/i)).not.toBeInTheDocument();
  });

  test("does NOT render banner when evaluation_mode is not edge_candidate", () => {
    vi.mocked(useEvaluation).mockReturnValue({
      data: {
        id: "eval-div",
        campaign_id: CAMPAIGN_ID,
        evaluated_at: "2026-04-10T12:00:00Z",
        loss_threshold_ratio: 0.02,
        stddev_weight: 1,
        evaluation_mode: "diversity",
        baseline_pair_count: 4,
        candidates_total: 2,
        candidates_good: 1,
        avg_improvement_ms: 12,
        max_hops: 2,
        vm_lookback_minutes: 15,
        results: { candidates: [], unqualified_reasons: {} },
      },
      isLoading: false,
      isError: false,
    } as unknown as ReturnType<typeof useEvaluation>);

    const campaign = {
      ...makeCampaign({ state: "evaluated", evaluation_mode: "diversity" }),
      source_agent_ids: ["agent-1"],
    } as unknown as Campaign;

    renderTab(campaign);

    expect(screen.queryByText(/only one source agent/i)).not.toBeInTheDocument();
  });

  test("does NOT render banner when no evaluation row exists", () => {
    const campaign = {
      ...makeCampaign({ state: "completed", evaluation_mode: "edge_candidate" }),
      source_agent_ids: ["agent-1"],
    } as unknown as Campaign;

    renderTab(campaign);

    expect(screen.queryByText(/only one source agent/i)).not.toBeInTheDocument();
  });
});
