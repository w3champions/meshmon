import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
  CAMPAIGNS_LIST_KEY,
  type Campaign,
  type CampaignMeasurementsPage,
  campaignKey,
  campaignPairsKey,
  campaignPreviewKey,
  useCampaign,
  useCampaignMeasurements,
  useCampaignsList,
  useCreateCampaign,
  useDeleteCampaign,
  useEditCampaign,
  useForcePair,
  usePatchCampaign,
  usePreviewDispatchCount,
  useStartCampaign,
  useStopCampaign,
} from "@/api/hooks/campaigns";

const CAMPAIGN: Campaign = {
  id: "11111111-1111-1111-1111-111111111111",
  title: "Paris uplink audit",
  notes: "Quarterly diversity sweep",
  state: "draft",
  evaluation_mode: "diversity",
  protocol: "icmp",
  force_measurement: false,
  loss_threshold_pct: 5,
  stddev_weight: 1,
  probe_count: 60,
  probe_count_detail: 10,
  probe_stagger_ms: 100,
  timeout_ms: 1_000,
  created_at: "2026-04-16T11:59:00Z",
};

function makeQueryClient(): QueryClient {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

function wrapWith(qc: QueryClient) {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

function wrap() {
  return wrapWith(makeQueryClient());
}

afterEach(() => vi.restoreAllMocks());

describe("useCampaignsList", () => {
  test("GETs /api/campaigns with query params and returns the data", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify([CAMPAIGN]), { status: 200 }));

    const { result } = renderHook(() => useCampaignsList({ q: "paris" }), {
      wrapper: wrap(),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toEqual([CAMPAIGN]);

    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).toContain("/api/campaigns");
    expect(url).toContain("q=paris");
  });
});

describe("useCampaign", () => {
  test("is disabled when id is undefined (no network call)", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(() => useCampaign(undefined), { wrapper: wrap() });
    // `enabled: false` — the query stays idle, so `isLoading` is `false`
    // and the query fn never dispatches a request.
    expect(result.current.fetchStatus).toBe("idle");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  test("returns null on 404", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "not_found" }), { status: 404 }),
    );

    const { result } = renderHook(() => useCampaign("abc"), { wrapper: wrap() });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toBeNull();
  });
});

describe("usePreviewDispatchCount", () => {
  test("is disabled when id is undefined", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(() => usePreviewDispatchCount(undefined), {
      wrapper: wrap(),
    });
    expect(result.current.fetchStatus).toBe("idle");
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});

describe("useCreateCampaign", () => {
  test("POSTs the body unchanged and invalidates the list key on success", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(CAMPAIGN), { status: 200 }));
    const qc = makeQueryClient();
    // Prime the list key so we can verify it gets flipped to invalidated.
    qc.setQueryData([...CAMPAIGNS_LIST_KEY, {}], [CAMPAIGN]);
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(() => useCreateCampaign(), { wrapper: wrapWith(qc) });

    const body = {
      title: "Paris uplink audit",
      notes: "Quarterly diversity sweep",
      protocol: "icmp" as const,
    };
    await act(async () => {
      result.current.mutate(body);
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const call = fetchSpy.mock.calls[0]?.[0];
    const request = call instanceof Request ? call : null;
    expect(request).not.toBeNull();
    expect(request?.method).toBe("POST");
    expect(request?.url).toContain("/api/campaigns");
    const sent = request ? await request.json() : null;
    expect(sent).toEqual(body);

    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: CAMPAIGNS_LIST_KEY });
  });
});

describe("useStartCampaign", () => {
  test("surfaces 409 illegal_state_transition through mutation.error", async () => {
    const errorBody = { error: "illegal_state_transition" };
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(errorBody), { status: 409 }),
    );
    const qc = makeQueryClient();

    const { result } = renderHook(() => useStartCampaign(), { wrapper: wrapWith(qc) });

    await act(async () => {
      result.current.mutate(CAMPAIGN.id);
    });

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error).toBeInstanceOf(Error);
    expect(result.current.error?.message).toContain("failed to start campaign");
    // openapi-fetch parses the 4xx body into `error` — it lands on `Error.cause`
    // so the caller can surface the server's machine-readable reason.
    expect(result.current.error?.cause).toEqual(errorBody);
  });
});

describe("useDeleteCampaign", () => {
  test("removes the entry cache on success", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(null, { status: 204 }));
    const qc = makeQueryClient();
    qc.setQueryData(campaignKey(CAMPAIGN.id), CAMPAIGN);
    const removeSpy = vi.spyOn(qc, "removeQueries");

    const { result } = renderHook(() => useDeleteCampaign(), { wrapper: wrapWith(qc) });

    await act(async () => {
      result.current.mutate(CAMPAIGN.id);
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(removeSpy).toHaveBeenCalledWith({ queryKey: campaignKey(CAMPAIGN.id) });
    expect(qc.getQueryData(campaignKey(CAMPAIGN.id))).toBeUndefined();
  });
});

describe("usePatchCampaign", () => {
  test("PATCHes the body and seeds the entry cache + invalidates the list", async () => {
    const updated: Campaign = { ...CAMPAIGN, title: "new title" };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(updated), { status: 200 }));
    const qc = makeQueryClient();
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(() => usePatchCampaign(), { wrapper: wrapWith(qc) });
    const body = { title: "new title" };
    await act(async () => {
      result.current.mutate({ id: CAMPAIGN.id, body });
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const request = fetchSpy.mock.calls[0]?.[0] as Request;
    expect(request.method).toBe("PATCH");
    expect(request.url).toContain(`/api/campaigns/${CAMPAIGN.id}`);
    expect(await request.json()).toEqual(body);

    expect(qc.getQueryData(campaignKey(CAMPAIGN.id))).toEqual(updated);
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: CAMPAIGNS_LIST_KEY });
  });
});

describe("useStopCampaign", () => {
  test("POSTs /stop, seeds the entry cache, and invalidates list + preview", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(CAMPAIGN), { status: 200 }));
    const qc = makeQueryClient();
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(() => useStopCampaign(), { wrapper: wrapWith(qc) });
    await act(async () => {
      result.current.mutate(CAMPAIGN.id);
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const request = fetchSpy.mock.calls[0]?.[0] as Request;
    expect(request.method).toBe("POST");
    expect(request.url).toContain(`/api/campaigns/${CAMPAIGN.id}/stop`);

    expect(qc.getQueryData(campaignKey(CAMPAIGN.id))).toEqual(CAMPAIGN);
    const invalidatedKeys = invalidateSpy.mock.calls.map((c) => c[0]?.queryKey);
    expect(invalidatedKeys).toContainEqual(CAMPAIGNS_LIST_KEY);
    expect(invalidatedKeys).toContainEqual(campaignPreviewKey(CAMPAIGN.id));
  });
});

describe("useEditCampaign", () => {
  test("POSTs the edit body unchanged and invalidates the three keys", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(CAMPAIGN), { status: 200 }));
    const qc = makeQueryClient();
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(() => useEditCampaign(), { wrapper: wrapWith(qc) });
    const body = {
      add_pairs: [{ destination_ip: "10.0.0.1", source_agent_id: "agent-a" }],
      remove_pairs: [],
      force_measurement: true,
    };
    await act(async () => {
      result.current.mutate({ id: CAMPAIGN.id, body });
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const request = fetchSpy.mock.calls[0]?.[0] as Request;
    expect(request.method).toBe("POST");
    expect(request.url).toContain(`/api/campaigns/${CAMPAIGN.id}/edit`);
    expect(await request.json()).toEqual(body);

    expect(qc.getQueryData(campaignKey(CAMPAIGN.id))).toEqual(CAMPAIGN);
    const invalidatedKeys = invalidateSpy.mock.calls.map((c) => c[0]?.queryKey);
    expect(invalidatedKeys).toContainEqual(CAMPAIGNS_LIST_KEY);
    expect(invalidatedKeys).toContainEqual(campaignPreviewKey(CAMPAIGN.id));
  });
});

describe("useCampaignMeasurements", () => {
  test("is disabled when id is undefined (no network call)", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(() => useCampaignMeasurements(undefined, {}), {
      wrapper: wrap(),
    });
    expect(result.current.fetchStatus).toBe("idle");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  test("threads filter params through the query string", async () => {
    const page: CampaignMeasurementsPage = { entries: [], next_cursor: null };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(page), { status: 200 }));

    const { result } = renderHook(
      () =>
        useCampaignMeasurements(CAMPAIGN.id, {
          resolution_state: "succeeded",
          protocol: "icmp",
          kind: "campaign",
          cursor: "abc",
          limit: 50,
        }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).toContain(`/api/campaigns/${CAMPAIGN.id}/measurements`);
    expect(url).toContain("resolution_state=succeeded");
    expect(url).toContain("protocol=icmp");
    expect(url).toContain("kind=campaign");
    expect(url).toContain("cursor=abc");
    expect(url).toContain("limit=50");
  });
});

describe("useForcePair", () => {
  test("seeds the entry cache and invalidates pairs + preview on success", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(CAMPAIGN), { status: 200 }),
    );
    const qc = makeQueryClient();
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(() => useForcePair(), { wrapper: wrapWith(qc) });

    await act(async () => {
      result.current.mutate({
        id: CAMPAIGN.id,
        body: {
          destination_ip: "10.0.0.1",
          source_agent_id: "agent-a",
        },
      });
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    // The server echo is written via `setQueryData` (no invalidate on the entry
    // key — that would trigger a pointless refetch of data we just stored).
    expect(qc.getQueryData(campaignKey(CAMPAIGN.id))).toEqual(CAMPAIGN);
    const invalidatedKeys = invalidateSpy.mock.calls.map((c) => c[0]?.queryKey);
    expect(invalidatedKeys).not.toContainEqual(campaignKey(CAMPAIGN.id));
    expect(invalidatedKeys).toContainEqual(campaignPairsKey(CAMPAIGN.id));
    expect(invalidatedKeys).toContainEqual(campaignPreviewKey(CAMPAIGN.id));
  });
});
