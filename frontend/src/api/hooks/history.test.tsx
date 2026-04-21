import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
  type HistoryDestination,
  type HistoryMeasurement,
  type HistorySource,
  useHistoryDestinations,
  useHistoryMeasurements,
  useHistorySources,
} from "@/api/hooks/history";

const SOURCE: HistorySource = {
  source_agent_id: "agent-a",
  display_name: "Agent A",
};

const DESTINATION: HistoryDestination = {
  destination_ip: "10.0.0.1",
  display_name: "10.0.0.1",
  is_mesh_member: true,
};

const MEASUREMENT: HistoryMeasurement = {
  id: 42,
  source_agent_id: "agent-a",
  destination_ip: "10.0.0.1",
  kind: "campaign",
  measured_at: "2026-04-20T00:00:00Z",
  probe_count: 60,
  protocol: "icmp",
  loss_pct: 0,
};

function makeQueryClient(): QueryClient {
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

describe("useHistorySources", () => {
  test("returns the list on 200", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify([SOURCE]), { status: 200 }),
    );

    const { result } = renderHook(() => useHistorySources(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toEqual([SOURCE]);
  });

  test("returns an empty array when the body is null", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response("null", { status: 200 }));

    const { result } = renderHook(() => useHistorySources(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data).toEqual([]);
  });
});

describe("useHistoryDestinations", () => {
  test("is disabled when source is undefined", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(() => useHistoryDestinations(undefined, undefined), {
      wrapper: wrap(),
    });
    expect(result.current.fetchStatus).toBe("idle");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  test("appends `q` only when set", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify([DESTINATION]), { status: 200 }));

    const { result } = renderHook(() => useHistoryDestinations("agent-a", "paris"), {
      wrapper: wrap(),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).toContain("source=agent-a");
    expect(url).toContain("q=paris");
  });

  test("surfaces 400 invalid source through mutation.error", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "unknown_source" }), { status: 400 }),
    );

    const { result } = renderHook(() => useHistoryDestinations("agent-a", undefined), {
      wrapper: wrap(),
    });
    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error?.message).toContain("failed to fetch history destinations");
  });
});

describe("useHistoryMeasurements", () => {
  test("is disabled when filter is null", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { result } = renderHook(() => useHistoryMeasurements(null), { wrapper: wrap() });
    expect(result.current.fetchStatus).toBe("idle");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  test("joins the protocol list into CSV", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify([MEASUREMENT]), { status: 200 }));

    const { result } = renderHook(
      () =>
        useHistoryMeasurements({
          source: "agent-a",
          destination: "10.0.0.1",
          protocols: ["icmp", "udp"],
        }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).toContain("protocols=icmp%2Cudp");
  });

  test("omits the protocols query param when the list is empty", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(JSON.stringify([]), { status: 200 }));

    renderHook(
      () =>
        useHistoryMeasurements({
          source: "agent-a",
          destination: "10.0.0.1",
          protocols: [],
        }),
      { wrapper: wrap() },
    );
    await waitFor(() => expect(fetchSpy).toHaveBeenCalled());
    const call = fetchSpy.mock.calls[0]?.[0];
    const url = call instanceof Request ? call.url : String(call);
    expect(url).not.toContain("protocols=");
  });
});
