import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { useAgent, useAgents } from "@/api/hooks/agents";

const AGENT = {
  id: "a",
  display_name: "Agent A",
  ip: "10.0.0.1",
  registered_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-04-16T11:59:00Z",
};

function wrap() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

afterEach(() => vi.restoreAllMocks());

describe("useAgents", () => {
  test("returns array body from /api/agents", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify([AGENT]), { status: 200 }),
    );
    const { result } = renderHook(() => useAgents(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.data).toEqual([AGENT]));
  });
});

describe("useAgent", () => {
  test("returns null on 404", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "nf" }), { status: 404 }),
    );
    const { result } = renderHook(() => useAgent("missing"), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toBeNull();
  });
});
