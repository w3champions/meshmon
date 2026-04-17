import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { usePathOverview } from "@/api/hooks/path-overview";

const OVERVIEW = {
  source: {
    id: "a",
    display_name: "Agent A",
    ip: "1.1.1.1",
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: "2026-04-13T11:59:00Z",
  },
  target: {
    id: "b",
    display_name: "Agent B",
    ip: "2.2.2.2",
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: "2026-04-13T11:59:00Z",
  },
  primary_protocol: "icmp",
  latest_by_protocol: { icmp: null, udp: null, tcp: null },
  recent_snapshots: [],
  metrics: {
    rtt_series: [[0, 185]],
    loss_series: [[0, 0.001]],
    rtt_current: 185,
    loss_current: 0.001,
  },
  window: { from: "2026-04-12T12:00:00Z", to: "2026-04-13T12:00:00Z" },
  step: "1m",
};

function wrap() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

afterEach(() => vi.restoreAllMocks());

describe("usePathOverview", () => {
  test("returns the overview body on 200", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      async () => new Response(JSON.stringify(OVERVIEW), { status: 200 }),
    );
    const { result } = renderHook(
      () => usePathOverview({ source: "a", target: "b", range: "24h" }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toEqual(OVERVIEW);
  });

  test("surfaces 404 as an error", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      async () => new Response(JSON.stringify({ error: "nf" }), { status: 404 }),
    );
    const { result } = renderHook(
      () => usePathOverview({ source: "a", target: "b", range: "24h" }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(result.current.isError).toBe(true));
  });

  test("passes ?protocol= through to the outgoing URL", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockImplementation(async () => new Response(JSON.stringify(OVERVIEW), { status: 200 }));
    const { result } = renderHook(
      () => usePathOverview({ source: "a", target: "b", range: "24h", protocol: "udp" }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : typeof call === "string" ? call : String(call);
    expect(url).toContain("/api/paths/a/b/overview");
    expect(url).toContain("protocol=udp");
    expect(url).toContain("from=");
    expect(url).toContain("to=");
  });
});
