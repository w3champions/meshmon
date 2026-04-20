import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { useCatalogueStream } from "@/api/hooks/catalogue-stream";

/** Minimal in-memory EventSource stand-in for deterministic tests. */
class MockEventSource {
  static instances: MockEventSource[] = [];
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: ((event?: unknown) => void) | null = null;
  onopen: ((event?: unknown) => void) | null = null;
  readyState = 1;

  constructor(public url: string) {
    MockEventSource.instances.push(this);
  }

  emit(payload: unknown): void {
    this.onmessage?.({ data: JSON.stringify(payload) });
  }

  raise(): void {
    this.onerror?.();
  }

  close(): void {
    this.readyState = 2;
  }
}

const ENTRY: CatalogueEntry = {
  id: "11111111-1111-1111-1111-111111111111",
  ip: "10.0.0.1",
  created_at: "2026-04-16T11:59:00Z",
  source: "operator",
  enrichment_status: "pending",
  operator_edited_fields: [],
};

function makeQueryClient(): QueryClient {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

function wrapWith(qc: QueryClient) {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

beforeEach(() => {
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useCatalogueStream", () => {
  test("opens /api/catalogue/stream on mount", () => {
    const qc = makeQueryClient();
    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    expect(MockEventSource.instances).toHaveLength(1);
    expect(MockEventSource.instances[0]?.url).toBe("/api/catalogue/stream");
  });

  test("invalidates list + facets on `created`", () => {
    const qc = makeQueryClient();
    qc.setQueryData(["catalogue", "list"], { entries: [], total: 0 });
    qc.setQueryData(["catalogue", "facets"], { country_code: [] });

    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    act(() => {
      MockEventSource.instances[0]?.emit({ kind: "created", id: ENTRY.id, ip: ENTRY.ip });
    });

    expect(qc.getQueryState(["catalogue", "list"])?.isInvalidated).toBe(true);
    expect(qc.getQueryState(["catalogue", "facets"])?.isInvalidated).toBe(true);
  });

  test("invalidates the entry + list on `updated`", () => {
    const qc = makeQueryClient();
    qc.setQueryData(["catalogue", "entry", ENTRY.id], ENTRY);
    qc.setQueryData(["catalogue", "list"], { entries: [ENTRY], total: 1 });

    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    act(() => {
      MockEventSource.instances[0]?.emit({ kind: "updated", id: ENTRY.id });
    });

    expect(qc.getQueryState(["catalogue", "entry", ENTRY.id])?.isInvalidated).toBe(true);
    expect(qc.getQueryState(["catalogue", "list"])?.isInvalidated).toBe(true);
  });

  test("patches entry cache in place on `enrichment_progress` without refetching", () => {
    const qc = makeQueryClient();
    qc.setQueryData(["catalogue", "entry", ENTRY.id], ENTRY);
    qc.setQueryData(["catalogue", "facets"], { enrichment_status: [] });

    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    act(() => {
      MockEventSource.instances[0]?.emit({
        kind: "enrichment_progress",
        id: ENTRY.id,
        status: "enriched",
      });
    });

    const cached = qc.getQueryData<CatalogueEntry>(["catalogue", "entry", ENTRY.id]);
    expect(cached?.enrichment_status).toBe("enriched");
    // In-place patch — the entry cache must NOT be flagged stale.
    expect(qc.getQueryState(["catalogue", "entry", ENTRY.id])?.isInvalidated).toBe(false);
    // Facet bucket for status changes — facets should be stale.
    expect(qc.getQueryState(["catalogue", "facets"])?.isInvalidated).toBe(true);
  });

  test("patches the list cache in place on `enrichment_progress`", () => {
    const qc = makeQueryClient();
    // Seed a list cache keyed by the full [CATALOGUE_LIST_KEY, query] shape
    qc.setQueryData(["catalogue", "list", {}], { entries: [ENTRY], total: 1 });
    qc.setQueryData(["catalogue", "facets"], { enrichment_status: [] });

    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    act(() => {
      MockEventSource.instances[0]?.emit({
        kind: "enrichment_progress",
        id: ENTRY.id,
        status: "enriched",
      });
    });

    const list = qc.getQueryData<{ entries: CatalogueEntry[]; total: number }>([
      "catalogue",
      "list",
      {},
    ]);
    expect(list?.entries[0]?.enrichment_status).toBe("enriched");
    // In-place patch — the list cache must NOT be flagged stale.
    expect(qc.getQueryState(["catalogue", "list", {}])?.isInvalidated).toBe(false);
  });

  test("list cache is untouched when no entry matches `enrichment_progress`", () => {
    const qc = makeQueryClient();
    const otherEntry: CatalogueEntry = { ...ENTRY, id: "22222222-2222-2222-2222-222222222222" };
    qc.setQueryData(["catalogue", "list", {}], { entries: [otherEntry], total: 1 });

    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    const beforeRef = qc.getQueryData(["catalogue", "list", {}]);
    act(() => {
      MockEventSource.instances[0]?.emit({
        kind: "enrichment_progress",
        id: ENTRY.id, // different from otherEntry.id
        status: "enriched",
      });
    });

    // Same reference — updater must return the original object when no entry matched
    const afterRef = qc.getQueryData(["catalogue", "list", {}]);
    expect(afterRef).toBe(beforeRef);
  });

  test("removes entry + invalidates list on `deleted`", () => {
    const qc = makeQueryClient();
    qc.setQueryData(["catalogue", "entry", ENTRY.id], ENTRY);
    qc.setQueryData(["catalogue", "list"], { entries: [ENTRY], total: 1 });
    qc.setQueryData(["catalogue", "facets"], { country_code: [] });

    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    act(() => {
      MockEventSource.instances[0]?.emit({ kind: "deleted", id: ENTRY.id });
    });

    expect(qc.getQueryData(["catalogue", "entry", ENTRY.id])).toBeUndefined();
    expect(qc.getQueryState(["catalogue", "list"])?.isInvalidated).toBe(true);
    expect(qc.getQueryState(["catalogue", "facets"])?.isInvalidated).toBe(true);
  });

  test("invalidates list + facets on `lag`", () => {
    const qc = makeQueryClient();
    qc.setQueryData(["catalogue", "list"], { entries: [], total: 0 });
    qc.setQueryData(["catalogue", "facets"], { country_code: [] });
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    act(() => {
      MockEventSource.instances[0]?.emit({ kind: "lag", missed: 7 });
    });

    expect(qc.getQueryState(["catalogue", "list"])?.isInvalidated).toBe(true);
    expect(qc.getQueryState(["catalogue", "facets"])?.isInvalidated).toBe(true);
    expect(warnSpy).toHaveBeenCalled();
  });

  test("reconnects with capped exponential backoff on error", () => {
    vi.useFakeTimers();
    const qc = makeQueryClient();
    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    expect(MockEventSource.instances).toHaveLength(1);

    // Trigger an error — no reconnect yet, a timer is scheduled.
    act(() => {
      MockEventSource.instances[0]?.raise();
    });
    expect(MockEventSource.instances).toHaveLength(1);

    // First delay is 1s.
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(MockEventSource.instances).toHaveLength(2);

    // Fail again → second delay is 2s.
    act(() => {
      MockEventSource.instances[1]?.raise();
    });
    act(() => {
      vi.advanceTimersByTime(1999);
    });
    expect(MockEventSource.instances).toHaveLength(2);
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(MockEventSource.instances).toHaveLength(3);
  });

  test("does not schedule multiple reconnects when onerror fires twice", () => {
    // Some browsers fire `onerror` more than once for the same dead
    // connection. Without a guard, each call would queue a fresh timer
    // and, after the delay elapses, spawn multiple concurrent
    // EventSource instances in parallel.
    vi.useFakeTimers();
    const qc = makeQueryClient();
    renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    expect(MockEventSource.instances).toHaveLength(1);

    // Two errors on the same connection, before the reconnect timer fires.
    act(() => {
      MockEventSource.instances[0]?.raise();
      MockEventSource.instances[0]?.raise();
    });
    // No reconnect yet — the single scheduled timer is still pending.
    expect(MockEventSource.instances).toHaveLength(1);

    // Advance well past any plausible backoff (cap is 30s). If the guard
    // is missing, the second `raise` will have scheduled a second timer
    // and we'll end up with 3 instances instead of 2.
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(MockEventSource.instances).toHaveLength(2);
  });

  test("cleans up EventSource + pending reconnect timer on unmount", () => {
    vi.useFakeTimers();
    const qc = makeQueryClient();
    const { unmount } = renderHook(() => useCatalogueStream(), { wrapper: wrapWith(qc) });
    const first = MockEventSource.instances[0];
    expect(first).toBeDefined();
    // Schedule a reconnect, then unmount before it fires.
    act(() => {
      first?.raise();
    });
    unmount();
    expect(first?.readyState).toBe(2);
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    // No new EventSource should be created after unmount.
    expect(MockEventSource.instances).toHaveLength(1);
  });
});
