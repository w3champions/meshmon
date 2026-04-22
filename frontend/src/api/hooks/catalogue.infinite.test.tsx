import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
  type CatalogueEntry,
  type CatalogueListResponse,
  type CatalogueMapResponse,
  useCatalogueListInfinite,
  useCatalogueMap,
} from "@/api/hooks/catalogue";
import { IpHostnameProvider } from "@/components/ip-hostname";

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

const ENTRY: CatalogueEntry = {
  id: "11111111-1111-1111-1111-111111111111",
  ip: "10.0.0.1",
  created_at: "2026-04-16T11:59:00Z",
  source: "operator",
  enrichment_status: "enriched",
  operator_edited_fields: [],
};

const ENTRY_B: CatalogueEntry = {
  ...ENTRY,
  id: "22222222-2222-2222-2222-222222222222",
  ip: "10.0.0.2",
};

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

function urlOf(call: unknown): string {
  if (!Array.isArray(call)) return "";
  const first = call[0];
  if (first instanceof Request) return first.url;
  return String(first);
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("useCatalogueListInfinite — paging", () => {
  test("advances the cursor on fetchNextPage", async () => {
    // First page: entries=[ENTRY], cursor="cursor-1".
    // Second page: entries=[ENTRY_B], cursor=null.
    const page1: CatalogueListResponse = {
      entries: [ENTRY],
      total: 2,
      next_cursor: "cursor-1",
    };
    const page2: CatalogueListResponse = {
      entries: [ENTRY_B],
      total: 2,
      next_cursor: null,
    };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(new Response(JSON.stringify(page1), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(page2), { status: 200 }));

    const { result } = renderHook(() => useCatalogueListInfinite(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.hasNextPage).toBe(true);

    await act(async () => {
      await result.current.fetchNextPage();
    });

    await waitFor(() => expect(result.current.data?.pages).toHaveLength(2));

    // Second fetch must carry the first page's cursor as `after`.
    const secondUrl = urlOf(fetchSpy.mock.calls[1]);
    expect(secondUrl).toContain("after=cursor-1");
    expect(result.current.data?.pages[1]).toEqual(page2);
  });

  test("terminates when next_cursor is null", async () => {
    const pageEnd: CatalogueListResponse = {
      entries: [ENTRY],
      total: 1,
      next_cursor: null,
    };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(pageEnd), { status: 200 }));

    const { result } = renderHook(() => useCatalogueListInfinite(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.hasNextPage).toBe(false);

    await act(async () => {
      await result.current.fetchNextPage();
    });
    // `fetchNextPage` is a no-op when `hasNextPage` is false — still just one
    // network call.
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    expect(result.current.data?.pages).toHaveLength(1);
  });

  test("treats missing next_cursor (undefined) as end-of-data", async () => {
    // Schema allows the server to omit `next_cursor` entirely; this must
    // behave the same as an explicit `null`.
    const pageEnd: CatalogueListResponse = { entries: [ENTRY], total: 1 };
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(pageEnd), { status: 200 }),
    );

    const { result } = renderHook(() => useCatalogueListInfinite(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.hasNextPage).toBe(false);
  });

  test("propagates pageSize into the `limit` query param", async () => {
    const pageEnd: CatalogueListResponse = { entries: [], total: 0, next_cursor: null };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(pageEnd), { status: 200 }));

    const { result } = renderHook(() => useCatalogueListInfinite({}, { pageSize: 42 }), {
      wrapper: wrap(),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const url = urlOf(fetchSpy.mock.calls[0]);
    expect(url).toContain("limit=42");
  });

  test("defaults pageSize to 100 when not provided", async () => {
    const pageEnd: CatalogueListResponse = { entries: [], total: 0, next_cursor: null };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(pageEnd), { status: 200 }));

    const { result } = renderHook(() => useCatalogueListInfinite(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const url = urlOf(fetchSpy.mock.calls[0]);
    expect(url).toContain("limit=100");
  });
});

describe("useCatalogueListInfinite — query key stability", () => {
  test("same filters reuse the cached query (no extra fetch)", async () => {
    const page: CatalogueListResponse = { entries: [ENTRY], total: 1, next_cursor: null };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(page), { status: 200 }));
    const qc = makeQueryClient();

    const { rerender, result } = renderHook(
      ({ q }: { q: { country_code?: string[] } }) => useCatalogueListInfinite(q),
      {
        wrapper: wrapWith(qc),
        initialProps: { q: { country_code: ["US"] } },
      },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchSpy).toHaveBeenCalledTimes(1);

    // Re-render with a new (but value-equal) filter object: query-key
    // structural equality must collapse this back onto the same cache entry.
    rerender({ q: { country_code: ["US"] } });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchSpy).toHaveBeenCalledTimes(1);
  });

  test("different filters spawn a separate cache entry + fetch", async () => {
    const page: CatalogueListResponse = { entries: [ENTRY], total: 1, next_cursor: null };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(page), { status: 200 }));
    const qc = makeQueryClient();

    const { rerender, result } = renderHook(
      ({ q }: { q: { country_code?: string[] } }) => useCatalogueListInfinite(q),
      {
        wrapper: wrapWith(qc),
        initialProps: { q: { country_code: ["US"] } },
      },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchSpy).toHaveBeenCalledTimes(1);

    rerender({ q: { country_code: ["DE"] } });
    // A fresh fetch must fire for the new filter.
    await waitFor(() => expect(fetchSpy).toHaveBeenCalledTimes(2));
    const secondUrl = urlOf(fetchSpy.mock.calls[1]);
    expect(secondUrl).toContain("country_code=DE");
  });
});

describe("useCatalogueMap", () => {
  test("is disabled when bbox is undefined — no fetch fires", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(() => useCatalogueMap(undefined, 5), { wrapper: wrap() });
    // `enabled: false` keeps the query in `pending` / fetchStatus `idle`.
    expect(result.current.fetchStatus).toBe("idle");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  test("fires with bbox CSV + zoom + filters when bbox provided", async () => {
    const response: CatalogueMapResponse = {
      kind: "detail",
      rows: [ENTRY],
      total: 1,
    };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(response), { status: 200 }));

    const bbox: [number, number, number, number] = [-10, -20, 30, 40];
    const { result } = renderHook(() => useCatalogueMap(bbox, 7, { country_code: ["US"] }), {
      wrapper: wrap(),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toEqual(response);

    const url = urlOf(fetchSpy.mock.calls[0]);
    expect(url).toContain("/api/catalogue/map");
    // `number[]` query params serialize as CSV via the client's `explode: false`
    // serializer — verify the bbox is comma-joined.
    expect(decodeURIComponent(url)).toContain("bbox=-10,-20,30,40");
    expect(url).toContain("zoom=7");
    expect(url).toContain("country_code=US");
  });

  test("narrows the response on kind discriminator", async () => {
    const clusterResponse: CatalogueMapResponse = {
      kind: "clusters",
      buckets: [
        {
          bbox: [0, 0, 1, 1],
          count: 42,
          lat: 0.5,
          lng: 0.5,
          sample_id: "33333333-3333-3333-3333-333333333333",
        },
      ],
      cell_size: 0.5,
      total: 42,
    };
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(clusterResponse), { status: 200 }),
    );

    const bbox: [number, number, number, number] = [0, 0, 1, 1];
    const { result } = renderHook(() => useCatalogueMap(bbox, 3), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const data = result.current.data;
    expect(data).toBeDefined();
    if (!data) return;
    // Type-narrowing proof: each branch touches a field that only exists on
    // its own variant. TypeScript fails compilation here if the discriminator
    // doesn't narrow correctly.
    if (data.kind === "detail") {
      expect(data.rows).toEqual([ENTRY]);
    } else {
      expect(data.kind).toBe("clusters");
      expect(data.buckets).toHaveLength(1);
      expect(data.cell_size).toBe(0.5);
    }
  });

  test("bbox change refires the query (new cache entry)", async () => {
    const response: CatalogueMapResponse = { kind: "detail", rows: [], total: 0 };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(response), { status: 200 }));

    const { rerender, result } = renderHook(
      ({ bbox }: { bbox: [number, number, number, number] }) => useCatalogueMap(bbox, 5),
      {
        wrapper: wrap(),
        initialProps: { bbox: [0, 0, 1, 1] as [number, number, number, number] },
      },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchSpy).toHaveBeenCalledTimes(1);

    rerender({ bbox: [2, 2, 3, 3] });
    await waitFor(() => expect(fetchSpy).toHaveBeenCalledTimes(2));
  });
});
