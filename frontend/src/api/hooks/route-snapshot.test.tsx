import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { useRouteSnapshot } from "@/api/hooks/route-snapshot";
import { IpHostnameProvider } from "@/components/ip-hostname";

const DETAIL = {
  id: 101,
  source_id: "a",
  target_id: "b",
  protocol: "icmp",
  observed_at: "2026-04-13T10:00:00Z",
  hops: [
    {
      position: 1,
      observed_ips: [{ ip: "10.0.0.1", freq: 1 }],
      avg_rtt_micros: 1_000,
      stddev_rtt_micros: 100,
      loss_ratio: 0,
    },
  ],
};

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

function wrap() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>
      <IpHostnameProvider>{children}</IpHostnameProvider>
    </QueryClientProvider>
  );
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("useRouteSnapshot", () => {
  test("disabled when id is undefined — no fetch", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(
      () => useRouteSnapshot({ source: "a", target: "b", id: undefined }),
      { wrapper: wrap() },
    );
    // Give React Query a chance to settle.
    await waitFor(() => expect(result.current.fetchStatus).toBe("idle"));
    expect(fetchSpy).not.toHaveBeenCalled();
    expect(result.current.data).toBeUndefined();
  });

  test("fetches snapshot by id", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      async () => new Response(JSON.stringify(DETAIL), { status: 200 }),
    );
    const { result } = renderHook(() => useRouteSnapshot({ source: "a", target: "b", id: 101 }), {
      wrapper: wrap(),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toEqual(DETAIL);
  });

  test("returns null on 404", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      async () => new Response(JSON.stringify({ error: "nf" }), { status: 404 }),
    );
    const { result } = renderHook(() => useRouteSnapshot({ source: "a", target: "b", id: 999 }), {
      wrapper: wrap(),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toBeNull();
  });
});
