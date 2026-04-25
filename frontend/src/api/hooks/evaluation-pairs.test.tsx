import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { campaignEvaluationKey } from "@/api/hooks/campaigns";
import {
  campaignEvaluationCandidatePairsKey,
  type EvaluationPairDetailListResponse,
  useCandidatePairDetails,
} from "@/api/hooks/evaluation-pairs";
import { IpHostnameProvider } from "@/components/ip-hostname";

const CAMPAIGN_ID = "11111111-1111-1111-1111-111111111111";
const DEST_IP = "10.0.0.1";

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

function wrapWith(qc: QueryClient) {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>
      <IpHostnameProvider>{children}</IpHostnameProvider>
    </QueryClientProvider>
  );
}

function makeQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
}

function makePage(
  entries: Array<{
    source_agent_id: string;
    destination_agent_id: string;
    destination_ip?: string;
  }>,
  next_cursor: string | null = null,
  total: number = entries.length,
): EvaluationPairDetailListResponse {
  return {
    entries: entries.map((e) => ({
      source_agent_id: e.source_agent_id,
      destination_agent_id: e.destination_agent_id,
      destination_ip: e.destination_ip ?? DEST_IP,
      direct_rtt_ms: 50,
      direct_stddev_ms: 1,
      direct_loss_ratio: 0.001,
      direct_source: "active_probe",
      transit_rtt_ms: 30,
      transit_stddev_ms: 0.5,
      transit_loss_ratio: 0.0005,
      improvement_ms: 20,
      qualifies: true,
    })),
    next_cursor,
    total,
  };
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("campaignEvaluationCandidatePairsKey", () => {
  test("prepends campaignEvaluationKey so SSE invalidation cascades", () => {
    const key = campaignEvaluationCandidatePairsKey(CAMPAIGN_ID, DEST_IP, {
      sort: "improvement_ms",
      dir: "desc",
    });
    const evalKey = campaignEvaluationKey(CAMPAIGN_ID);
    // The first N elements of the pair-details key match the
    // evaluation key — TanStack Query's `invalidateQueries` walks the
    // tree by prefix, so cascading invalidation only works when this
    // structural relationship holds.
    expect(key.slice(0, evalKey.length)).toEqual([...evalKey]);
    expect(key).toContain("candidates");
    expect(key).toContain("pair_details");
    expect(key).toContain(DEST_IP);
  });

  test("encodes sort + dir into the queryKey so a sort change spawns a fresh query", () => {
    const a = campaignEvaluationCandidatePairsKey(CAMPAIGN_ID, DEST_IP, {
      sort: "improvement_ms",
      dir: "desc",
    });
    const b = campaignEvaluationCandidatePairsKey(CAMPAIGN_ID, DEST_IP, {
      sort: "improvement_ms",
      dir: "asc",
    });
    expect(a).not.toEqual(b);
  });
});

describe("useCandidatePairDetails", () => {
  test("returns undefined as next_cursor when end-of-pages is reached", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(
        JSON.stringify(makePage([{ source_agent_id: "a", destination_agent_id: "b" }])),
        {
          status: 200,
        },
      ),
    );
    const { result } = renderHook(
      () => useCandidatePairDetails(CAMPAIGN_ID, DEST_IP, { sort: "improvement_ms", dir: "desc" }),
      { wrapper: wrapWith(makeQueryClient()) },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.hasNextPage).toBe(false);
  });

  test("disabled when campaignId or destinationIp is undefined", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(
      () => useCandidatePairDetails(undefined, DEST_IP, { sort: "improvement_ms", dir: "desc" }),
      { wrapper: wrapWith(makeQueryClient()) },
    );
    expect(result.current.fetchStatus).toBe("idle");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  test("forwards sort + dir + filters as query params", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(
        JSON.stringify(makePage([{ source_agent_id: "a", destination_agent_id: "b" }])),
        {
          status: 200,
        },
      ),
    );

    const { result } = renderHook(
      () =>
        useCandidatePairDetails(CAMPAIGN_ID, DEST_IP, {
          sort: "transit_rtt_ms",
          dir: "asc",
          min_improvement_ms: 5,
          qualifies_only: true,
        }),
      { wrapper: wrapWith(makeQueryClient()) },
    );

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).toContain("sort=transit_rtt_ms");
    expect(url).toContain("dir=asc");
    expect(url).toContain("min_improvement_ms=5");
    expect(url).toContain("qualifies_only=true");
  });

  test("omits null filters from the wire query string", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(makePage([])), { status: 200 }));

    renderHook(
      () =>
        useCandidatePairDetails(CAMPAIGN_ID, DEST_IP, {
          sort: "improvement_ms",
          dir: "desc",
          min_improvement_ms: null,
          min_improvement_ratio: null,
          max_transit_rtt_ms: null,
          max_transit_stddev_ms: null,
          qualifies_only: null,
        }),
      { wrapper: wrapWith(makeQueryClient()) },
    );

    await waitFor(() => expect(fetchSpy).toHaveBeenCalled());
    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).not.toContain("min_improvement_ms");
    expect(url).not.toContain("min_improvement_ratio");
    expect(url).not.toContain("max_transit_rtt_ms");
    expect(url).not.toContain("max_transit_stddev_ms");
    expect(url).not.toContain("qualifies_only");
  });
});
