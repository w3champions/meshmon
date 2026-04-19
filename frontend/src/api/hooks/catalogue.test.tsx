import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
  type CatalogueEntry,
  type CatalogueListResponse,
  type CataloguePasteResponse,
  useCatalogueEntry,
  useCatalogueList,
  useDeleteCatalogueEntry,
  usePasteCatalogue,
  usePatchCatalogueEntry,
  useReenrichMany,
  useReenrichOne,
} from "@/api/hooks/catalogue";

const ENTRY: CatalogueEntry = {
  id: "11111111-1111-1111-1111-111111111111",
  ip: "10.0.0.1",
  created_at: "2026-04-16T11:59:00Z",
  source: "operator",
  enrichment_status: "enriched",
  operator_edited_fields: [],
  display_name: "Alpha",
  country_code: "US",
  country_name: "United States",
  city: "San Francisco",
  network_operator: "ExampleNet",
  asn: 64500,
};

const LIST_OK: CatalogueListResponse = {
  entries: [ENTRY],
  total: 1,
};

function makeQueryClient() {
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

describe("useCatalogueList", () => {
  test("returns the list body on 200", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(LIST_OK), { status: 200 }),
    );
    const { result } = renderHook(() => useCatalogueList(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toEqual(LIST_OK);
  });

  test("surfaces 500 as an error", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "boom" }), { status: 500 }),
    );
    const { result } = renderHook(() => useCatalogueList(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error?.message).toContain("failed to fetch catalogue");
  });

  test("forwards filter query params to the outgoing URL", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(LIST_OK), { status: 200 }));
    const { result } = renderHook(
      () => useCatalogueList({ country_code: ["US", "DE"], limit: 25 }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).toContain("/api/catalogue");
    expect(url).toContain("country_code=US");
    expect(url).toContain("country_code=DE");
    expect(url).toContain("limit=25");
  });
});

describe("useCatalogueEntry", () => {
  test("returns null on 404", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "not_found" }), { status: 404 }),
    );
    const { result } = renderHook(() => useCatalogueEntry("missing"), {
      wrapper: wrap(),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toBeNull();
  });
});

describe("usePatchCatalogueEntry", () => {
  test("optimistically updates the entry cache and reconciles on success", async () => {
    const patched: CatalogueEntry = {
      ...ENTRY,
      display_name: "Beta",
      operator_edited_fields: ["DisplayName"],
    };
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(patched), { status: 200 }),
    );
    const qc = makeQueryClient();
    qc.setQueryData(["catalogue", "entry", ENTRY.id], ENTRY);

    const { result } = renderHook(() => usePatchCatalogueEntry(), {
      wrapper: wrapWith(qc),
    });

    await act(async () => {
      result.current.mutate({ id: ENTRY.id, patch: { display_name: "Beta" } });
    });

    // Optimistic path: cache should reflect the patch before the response
    // arrives, and the server echo should replace it on success.
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const cached = qc.getQueryData<CatalogueEntry>(["catalogue", "entry", ENTRY.id]);
    expect(cached).toEqual(patched);
  });

  test("rolls back the optimistic update on 500", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "boom" }), { status: 500 }),
    );
    const qc = makeQueryClient();
    qc.setQueryData(["catalogue", "entry", ENTRY.id], ENTRY);

    const { result } = renderHook(() => usePatchCatalogueEntry(), {
      wrapper: wrapWith(qc),
    });

    await act(async () => {
      result.current.mutate({ id: ENTRY.id, patch: { display_name: "oops" } });
    });

    // After failure, the cache must contain the original entry — not the
    // optimistic patch that was briefly written by onMutate.
    await waitFor(() => expect(result.current.isError).toBe(true));
    const cached = qc.getQueryData<CatalogueEntry>(["catalogue", "entry", ENTRY.id]);
    expect(cached).toEqual(ENTRY);
    expect(cached?.display_name).toBe("Alpha");
  });
});

describe("usePasteCatalogue", () => {
  test("POSTs to /api/catalogue and invalidates list + facets caches on success", async () => {
    const pasteResponse: CataloguePasteResponse = {
      created: [],
      existing: [],
      invalid: [],
    };
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify(pasteResponse), { status: 200 }));
    const qc = makeQueryClient();
    // Prime both caches so we can verify they are marked stale after mutate.
    qc.setQueryData(["catalogue", "list"], { entries: [], total: 0 });
    qc.setQueryData(["catalogue", "facets"], { country_code: [], enrichment_status: [] });

    const { result } = renderHook(() => usePasteCatalogue(), { wrapper: wrapWith(qc) });

    await act(async () => {
      result.current.mutate({ ips: ["1.1.1.1"] });
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    // Verify outgoing HTTP request shape.
    const call = fetchSpy.mock.calls[0]?.[0];
    const request = call instanceof Request ? call : null;
    expect(request).not.toBeNull();
    expect(request?.method).toBe("POST");
    expect(request?.url).toContain("/api/catalogue");
    const body = request ? await request.json() : null;
    expect(body).toEqual({ ips: ["1.1.1.1"] });

    // Both primed caches should be flagged stale by the invalidateQueries call.
    const listState = qc.getQueryState(["catalogue", "list"]);
    const facetsState = qc.getQueryState(["catalogue", "facets"]);
    expect(listState?.isInvalidated).toBe(true);
    expect(facetsState?.isInvalidated).toBe(true);
  });
});

describe("useDeleteCatalogueEntry", () => {
  test("issues DELETE and clears the entry cache on success", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(null, { status: 204 }));
    const qc = makeQueryClient();
    qc.setQueryData(["catalogue", "entry", ENTRY.id], ENTRY);

    const { result } = renderHook(() => useDeleteCatalogueEntry(), {
      wrapper: wrapWith(qc),
    });

    await act(async () => {
      result.current.mutate(ENTRY.id);
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const call = fetchSpy.mock.calls[0]?.[0];
    const method = call instanceof Request ? call.method : "";
    expect(method).toBe("DELETE");
    expect(qc.getQueryData(["catalogue", "entry", ENTRY.id])).toBeUndefined();
  });
});

describe("useReenrichOne", () => {
  test("POSTs to /api/catalogue/{id}/reenrich with no body", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(null, { status: 202 }));

    const { result } = renderHook(() => useReenrichOne(), { wrapper: wrap() });

    await act(async () => {
      result.current.mutate(ENTRY.id);
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const call = fetchSpy.mock.calls[0]?.[0];
    const request = call instanceof Request ? call : null;
    expect(request).not.toBeNull();
    expect(request?.method).toBe("POST");
    expect(request?.url).toContain(`/api/catalogue/${ENTRY.id}/reenrich`);
    const body = request ? await request.text() : "";
    expect(body).toBe("");
  });
});

describe("useReenrichMany", () => {
  test("POSTs to /api/catalogue/reenrich with { ids }", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(null, { status: 202 }));

    const { result } = renderHook(() => useReenrichMany(), { wrapper: wrap() });
    const ids = ["id-a", "id-b"];

    await act(async () => {
      result.current.mutate({ ids });
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const call = fetchSpy.mock.calls[0]?.[0];
    const request = call instanceof Request ? call : null;
    expect(request).not.toBeNull();
    expect(request?.method).toBe("POST");
    expect(request?.url).toContain("/api/catalogue/reenrich");
    const body = request ? await request.json() : null;
    expect(body).toEqual({ ids });
  });
});
