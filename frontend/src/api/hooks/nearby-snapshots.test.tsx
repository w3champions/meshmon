import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { useNearbySnapshots } from "@/api/hooks/nearby-snapshots";

function wrap() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

function summary(id: number, observed_at: string) {
  return { id, source_id: "a", target_id: "b", protocol: "icmp", observed_at };
}

function page(items: ReturnType<typeof summary>[]) {
  return { items, limit: 500, offset: 0 };
}

afterEach(() => vi.restoreAllMocks());

describe("useNearbySnapshots", () => {
  test("fetches list_routes with a window around aroundTimeMs", async () => {
    const captured: string[] = [];
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = typeof input === "string" ? input : (input as Request).url;
      captured.push(url);
      return new Response(
        JSON.stringify(
          page([
            summary(1, "2026-04-17T09:00:00Z"),
            summary(2, "2026-04-17T09:05:00Z"),
            summary(3, "2026-04-17T09:12:04Z"),
            summary(4, "2026-04-17T09:14:41Z"),
            summary(5, "2026-04-17T09:20:00Z"),
          ]),
        ),
        { status: 200 },
      );
    });

    const aroundTimeMs = Date.UTC(2026, 3, 17, 9, 13, 0);
    const { result } = renderHook(
      () =>
        useNearbySnapshots({
          source: "a",
          target: "b",
          protocol: "icmp",
          aroundTimeMs,
        }),
      { wrapper: wrap() },
    );

    await waitFor(() => expect(result.current.isLoading).toBe(false));

    const first = captured[0];
    expect(first).toContain("/api/paths/a/b/routes");
    expect(first).toContain("protocol=icmp");
    expect(first).toMatch(/from=/);
    expect(first).toMatch(/to=/);

    expect(result.current.snapshots.map((s) => s.id)).toEqual([1, 2, 3, 4, 5]);
  });

  test("findClosest picks the snapshot with minimum |observed_at - target|", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      async () =>
        new Response(
          JSON.stringify(
            page([
              summary(1, "2026-04-17T09:10:00Z"),
              summary(2, "2026-04-17T09:12:00Z"),
              summary(3, "2026-04-17T09:14:00Z"),
            ]),
          ),
        ),
    );
    const aroundTimeMs = Date.UTC(2026, 3, 17, 9, 12, 0);
    const { result } = renderHook(
      () => useNearbySnapshots({ source: "a", target: "b", protocol: "icmp", aroundTimeMs }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(result.current.isLoading).toBe(false));

    const target = Date.UTC(2026, 3, 17, 9, 13, 29);
    expect(result.current.findClosest(target)?.id).toBe(3);
    expect(result.current.findClosest(Date.UTC(2026, 3, 17, 9, 10, 59))?.id).toBe(1);
  });

  test("getNeighbors returns the time-order prev/next for an id", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      async () =>
        new Response(
          JSON.stringify(
            page([
              summary(1, "2026-04-17T09:10:00Z"),
              summary(2, "2026-04-17T09:12:00Z"),
              summary(3, "2026-04-17T09:14:00Z"),
            ]),
          ),
        ),
    );
    const aroundTimeMs = Date.UTC(2026, 3, 17, 9, 12, 0);
    const { result } = renderHook(
      () => useNearbySnapshots({ source: "a", target: "b", protocol: "icmp", aroundTimeMs }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(result.current.isLoading).toBe(false));

    const neighbors = result.current.getNeighbors(2);
    expect(neighbors.prev?.id).toBe(1);
    expect(neighbors.next?.id).toBe(3);

    const edge = result.current.getNeighbors(1);
    expect(edge.prev).toBeUndefined();
    expect(edge.next?.id).toBe(2);
  });

  test("widens the window when fewer than 3 neighbors exist on a side", async () => {
    const calls: { fromMs: number; toMs: number }[] = [];
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = new URL(typeof input === "string" ? input : (input as Request).url);
      const fromMs = Date.parse(url.searchParams.get("from") ?? "");
      const toMs = Date.parse(url.searchParams.get("to") ?? "");
      calls.push({ fromMs, toMs });
      return new Response(JSON.stringify(page([summary(1, "2026-04-17T09:12:00Z")])), {
        status: 200,
      });
    });
    const aroundTimeMs = Date.UTC(2026, 3, 17, 9, 12, 0);
    renderHook(
      () => useNearbySnapshots({ source: "a", target: "b", protocol: "icmp", aroundTimeMs }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(calls.length).toBeGreaterThanOrEqual(2));
    expect(calls[1].toMs - calls[1].fromMs).toBeGreaterThan(calls[0].toMs - calls[0].fromMs);
  });
});
