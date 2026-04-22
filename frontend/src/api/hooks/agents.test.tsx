import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { useAgent, useAgents } from "@/api/hooks/agents";
import { IpHostnameProvider, useIpHostname } from "@/components/ip-hostname";

const AGENT = {
  id: "a",
  display_name: "Agent A",
  ip: "10.0.0.1",
  registered_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-04-16T11:59:00Z",
};

const AGENT_WITH_HOSTNAME = { ...AGENT, hostname: "a.example.com" };

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

describe("useAgents", () => {
  test("returns array body from /api/agents", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify([AGENT]), { status: 200 }),
    );
    const { result } = renderHook(() => useAgents(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.data).toEqual([AGENT]));
  });

  test("seeds the hostname provider from the response", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify([AGENT_WITH_HOSTNAME]), { status: 200 }),
    );

    const { result } = renderHook(
      () => {
        const agents = useAgents();
        const hostname = useIpHostname("10.0.0.1");
        return { agents, hostname };
      },
      { wrapper: wrap() },
    );

    await waitFor(() => expect(result.current.agents.isSuccess).toBe(true));
    // Seeded via useSeedHostnamesOnResponse — no SSE tick required.
    await waitFor(() => expect(result.current.hostname).toBe("a.example.com"));
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

  test("seeds the hostname provider with the single-agent response", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(AGENT_WITH_HOSTNAME), { status: 200 }),
    );

    const { result } = renderHook(
      () => {
        const agent = useAgent("a");
        const hostname = useIpHostname("10.0.0.1");
        return { agent, hostname };
      },
      { wrapper: wrap() },
    );

    await waitFor(() => expect(result.current.agent.isSuccess).toBe(true));
    await waitFor(() => expect(result.current.hostname).toBe("a.example.com"));
  });

  test("404 response seeds nothing (no throw)", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "nf" }), { status: 404 }),
    );
    const { result } = renderHook(
      () => {
        const agent = useAgent("missing");
        const hostname = useIpHostname("10.0.0.1");
        return { agent, hostname };
      },
      { wrapper: wrap() },
    );
    await waitFor(() => expect(result.current.agent.isSuccess).toBe(true));
    // Give the effect a tick to settle before asserting no side-effect.
    act(() => {});
    expect(result.current.hostname).toBeUndefined();
  });
});
