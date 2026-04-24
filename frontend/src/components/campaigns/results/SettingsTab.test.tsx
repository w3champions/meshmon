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
    created_at: overrides.created_at ?? "2026-04-01T12:00:00Z",
    created_by: overrides.created_by ?? "alice",
    started_at: overrides.started_at ?? null,
    stopped_at: overrides.stopped_at ?? null,
    completed_at: overrides.completed_at ?? null,
    evaluated_at: overrides.evaluated_at ?? null,
    pair_counts: overrides.pair_counts ?? [],
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

  test("surfaces a vm_not_configured toast when the deployment lacks VM", async () => {
    const user = userEvent.setup();
    renderTab(makeCampaign({ state: "completed" }));

    patchStub.mutate.mockImplementation(
      (_vars: unknown, opts?: { onSuccess?: (result: unknown) => void }) => {
        opts?.onSuccess?.({});
      },
    );
    const err = new Error("failed", { cause: { error: "vm_not_configured" } });
    evaluateStub.mutate.mockImplementation(
      (_id: string, opts?: { onError?: (err: Error) => void }) => {
        opts?.onError?.(err);
      },
    );

    await user.click(screen.getByRole("button", { name: /re-evaluate/i }));

    await waitFor(() => {
      expect(screen.getByText(/victoriametrics isn't configured/i)).toBeInTheDocument();
    });
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
