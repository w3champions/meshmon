import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
  campaignEvaluationKey,
  campaignKey,
  campaignMeasurementsPrefixKey,
  campaignPairsKey,
  campaignPreviewKey,
} from "@/api/hooks/campaigns";
import {
  type Evaluation,
  useEvaluateCampaign,
  useEvaluation,
  useTriggerDetail,
} from "@/api/hooks/evaluation";
import { IpHostnameProvider } from "@/components/ip-hostname";

const CAMPAIGN_ID = "11111111-1111-1111-1111-111111111111";

const EVALUATION: Evaluation = {
  campaign_id: CAMPAIGN_ID,
  evaluated_at: "2026-04-20T00:00:00Z",
  loss_threshold_ratio: 0.05,
  stddev_weight: 1,
  evaluation_mode: "diversity",
  baseline_pair_count: 4,
  candidates_total: 2,
  candidates_good: 1,
  avg_improvement_ms: 12,
  results: {
    candidates: [],
    unqualified_reasons: {},
  },
};

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

function makeQueryClient(): QueryClient {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

function wrapWith(qc: QueryClient) {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>
      <IpHostnameProvider>{children}</IpHostnameProvider>
    </QueryClientProvider>
  );
}

function wrap() {
  return wrapWith(makeQueryClient());
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("useEvaluation", () => {
  test("returns null on 404 (campaign not evaluated)", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify({ error: "not_evaluated" }), { status: 404 }));

    const { result } = renderHook(() => useEvaluation(CAMPAIGN_ID), {
      wrapper: wrap(),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toBeNull();

    const request = fetchSpy.mock.calls[0]?.[0] as Request;
    expect(request.method).toBe("GET");
    expect(request.url).toMatch(new RegExp(`/api/campaigns/${CAMPAIGN_ID}/evaluation$`));
  });

  test("is disabled when id is undefined", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(() => useEvaluation(undefined), { wrapper: wrap() });
    expect(result.current.fetchStatus).toBe("idle");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  test("returns the evaluation row on 200", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(EVALUATION), { status: 200 }));

    const { result } = renderHook(() => useEvaluation(CAMPAIGN_ID), {
      wrapper: wrap(),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toEqual(EVALUATION);

    const request = fetchSpy.mock.calls[0]?.[0] as Request;
    expect(request.method).toBe("GET");
    expect(request.url).toMatch(new RegExp(`/api/campaigns/${CAMPAIGN_ID}/evaluation$`));
  });
});

describe("useEvaluateCampaign", () => {
  test("seeds the evaluation cache from the response and invalidates dependent views", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(EVALUATION), { status: 200 }));
    const qc = makeQueryClient();
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(() => useEvaluateCampaign(), {
      wrapper: wrapWith(qc),
    });
    await act(async () => {
      result.current.mutate(CAMPAIGN_ID);
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const request = fetchSpy.mock.calls[0]?.[0] as Request;
    expect(request.method).toBe("POST");
    expect(request.url).toMatch(new RegExp(`/api/campaigns/${CAMPAIGN_ID}/evaluate$`));

    // Evaluation cache is seeded directly from the mutation response, so
    // the evaluation key itself is NOT invalidated (avoids a refetch
    // round-trip). All other dependent views ARE invalidated so the
    // Pairs tab, Raw tab, and preview mirror the fresh state — parallel
    // to what `useTriggerDetail` does.
    expect(qc.getQueryData(campaignEvaluationKey(CAMPAIGN_ID))).toEqual(EVALUATION);
    const invalidatedKeys = invalidateSpy.mock.calls.map((c) => c[0]?.queryKey);
    expect(invalidatedKeys).toContainEqual(campaignKey(CAMPAIGN_ID));
    expect(invalidatedKeys).toContainEqual(campaignPairsKey(CAMPAIGN_ID));
    expect(invalidatedKeys).toContainEqual(campaignPreviewKey(CAMPAIGN_ID));
    expect(invalidatedKeys).toContainEqual(campaignMeasurementsPrefixKey(CAMPAIGN_ID));
    expect(invalidatedKeys).not.toContainEqual(campaignEvaluationKey(CAMPAIGN_ID));
  });
});

describe("useTriggerDetail", () => {
  test("invalidates pairs, preview, and the measurements prefix", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ campaign_state: "running", pairs_enqueued: 4 }), {
        status: 200,
      }),
    );
    const qc = makeQueryClient();
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(() => useTriggerDetail(), { wrapper: wrapWith(qc) });
    const body = { scope: "good_candidates" as const };
    await act(async () => {
      result.current.mutate({ id: CAMPAIGN_ID, body });
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const request = fetchSpy.mock.calls[0]?.[0] as Request;
    expect(request.method).toBe("POST");
    expect(request.url).toMatch(new RegExp(`/api/campaigns/${CAMPAIGN_ID}/detail$`));
    expect(await request.json()).toEqual(body);

    const invalidatedKeys = invalidateSpy.mock.calls.map((c) => c[0]?.queryKey);
    expect(invalidatedKeys).toContainEqual(campaignKey(CAMPAIGN_ID));
    expect(invalidatedKeys).toContainEqual(campaignPairsKey(CAMPAIGN_ID));
    expect(invalidatedKeys).toContainEqual(campaignPreviewKey(CAMPAIGN_ID));
    expect(invalidatedKeys).toContainEqual(campaignMeasurementsPrefixKey(CAMPAIGN_ID));
  });

  test("surfaces 409 illegal_state_transition via mutation.error", async () => {
    const errorBody = { error: "illegal_state_transition" };
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(errorBody), { status: 409 }),
    );
    const qc = makeQueryClient();

    const { result } = renderHook(() => useTriggerDetail(), { wrapper: wrapWith(qc) });
    await act(async () => {
      result.current.mutate({ id: CAMPAIGN_ID, body: { scope: "all" } });
    });
    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error?.cause).toEqual(errorBody);
  });
});
