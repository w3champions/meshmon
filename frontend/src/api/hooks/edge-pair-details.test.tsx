import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { campaignEdgePairsKey, campaignEdgePairsPrefixKey } from "@/api/hooks/campaigns";
import { type EdgePairsListResponse, useEdgePairDetails } from "@/api/hooks/evaluation";
import { IpHostnameProvider } from "@/components/ip-hostname";

const CAMPAIGN_ID = "22222222-2222-2222-2222-222222222222";

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

function makePage(count: number, next_cursor: string | null = null): EdgePairsListResponse {
  const entries = Array.from({ length: count }, (_, i) => ({
    candidate_ip: `10.0.0.${i + 1}`,
    destination_agent_id: `agent-${i}`,
    best_route_ms: 10 + i,
    best_route_loss_ratio: 0,
    best_route_stddev_ms: 1,
    best_route_kind: "direct" as const,
    best_route_legs: [],
    best_route_intermediaries: [] as string[],
    qualifies_under_t: true,
    is_unreachable: false,
  }));
  return { entries, next_cursor, total: count };
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("campaignEdgePairsPrefixKey", () => {
  test("is a prefix of campaignEdgePairsKey for the same campaign", () => {
    const prefix = campaignEdgePairsPrefixKey(CAMPAIGN_ID);
    const full = campaignEdgePairsKey(CAMPAIGN_ID, { sort: "best_route_ms", dir: "asc" });
    expect([...full].slice(0, prefix.length)).toEqual([...prefix]);
  });

  test("encodes query into key so different filters yield distinct cache entries", () => {
    const a = campaignEdgePairsKey(CAMPAIGN_ID, { sort: "best_route_ms", dir: "asc" });
    const b = campaignEdgePairsKey(CAMPAIGN_ID, { sort: "best_route_ms", dir: "desc" });
    expect(a).not.toEqual(b);
  });
});

describe("useEdgePairDetails", () => {
  test("is disabled when campaignId is undefined", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(() => useEdgePairDetails(undefined, {}), {
      wrapper: wrapWith(makeQueryClient()),
    });
    expect(result.current.fetchStatus).toBe("idle");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  test("fetches the first page and exposes entries", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(makePage(3)), { status: 200 }),
    );

    const { result } = renderHook(() => useEdgePairDetails(CAMPAIGN_ID, {}), {
      wrapper: wrapWith(makeQueryClient()),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.pages[0]?.entries).toHaveLength(3);
    expect(result.current.hasNextPage).toBe(false);
  });

  test("forwards sort + dir + candidate_ip as query params", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(makePage(1)), { status: 200 }));

    const { result } = renderHook(
      () =>
        useEdgePairDetails(CAMPAIGN_ID, {
          sort: "candidate_ip",
          dir: "desc",
          candidate_ip: "10.0.0.5",
          qualifies_only: true,
          reachable_only: true,
        }),
      { wrapper: wrapWith(makeQueryClient()) },
    );

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).toContain("sort=candidate_ip");
    expect(url).toContain("dir=desc");
    expect(url).toContain("candidate_ip=10.0.0.5");
    expect(url).toContain("qualifies_only=true");
    expect(url).toContain("reachable_only=true");
  });

  test("omits undefined query params from the wire", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(makePage(0)), { status: 200 }));

    renderHook(() => useEdgePairDetails(CAMPAIGN_ID, {}), {
      wrapper: wrapWith(makeQueryClient()),
    });

    await waitFor(() => expect(fetchSpy).toHaveBeenCalled());
    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).not.toContain("sort=");
    expect(url).not.toContain("dir=");
    expect(url).not.toContain("candidate_ip=");
    expect(url).not.toContain("qualifies_only=");
    expect(url).not.toContain("reachable_only=");
  });

  test("hasNextPage is true when next_cursor is returned", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(makePage(2, "cursor-abc")), { status: 200 }),
    );

    const { result } = renderHook(() => useEdgePairDetails(CAMPAIGN_ID, {}), {
      wrapper: wrapWith(makeQueryClient()),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.hasNextPage).toBe(true);
  });
});
