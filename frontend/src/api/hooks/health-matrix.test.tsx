import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { useHealthMatrix } from "@/api/hooks/health-matrix";

function vmResponse(
  results: Array<{
    source: string;
    target: string;
    value: string;
  }>,
) {
  return {
    status: "success",
    data: {
      resultType: "vector",
      result: results.map((r) => ({
        metric: { source: r.source, target: r.target },
        value: [1744804800, r.value],
      })),
    },
  };
}

function wrap() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

afterEach(() => vi.restoreAllMocks());

describe("useHealthMatrix", () => {
  test("one series (source=a, target=b) at 0.1 → degraded", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(vmResponse([{ source: "a", target: "b", value: "0.1" }])), {
        status: 200,
      }),
    );
    const { result } = renderHook(() => useHealthMatrix(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const matrix = result.current.data;
    expect(matrix).toBeDefined();
    expect(matrix?.size).toBe(1);
    const entry = matrix?.get("a>b");
    expect(entry).toBeDefined();
    expect(entry?.state).toBe("degraded");
    expect(entry?.failureRate).toBeCloseTo(0.1);
  });

  test("empty result array → empty matrix", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(vmResponse([])), { status: 200 }),
    );
    const { result } = renderHook(() => useHealthMatrix(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.size).toBe(0);
  });

  test("503 response → empty matrix (soft-fail)", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "vm not configured" }), { status: 503 }),
    );
    const { result } = renderHook(() => useHealthMatrix(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.size).toBe(0);
  });

  test("two series same (source, target) at 0.1 and 0.3 → keeps higher (0.3, unreachable)", async () => {
    const body = {
      status: "success",
      data: {
        resultType: "vector",
        result: [
          { metric: { source: "a", target: "b" }, value: [1744804800, "0.1"] },
          { metric: { source: "a", target: "b" }, value: [1744804800, "0.3"] },
        ],
      },
    };
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify(body), { status: 200 }),
    );
    const { result } = renderHook(() => useHealthMatrix(), { wrapper: wrap() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const matrix = result.current.data;
    expect(matrix?.size).toBe(1);
    const entry = matrix?.get("a>b");
    expect(entry?.failureRate).toBeCloseTo(0.3);
    expect(entry?.state).toBe("unreachable");
  });
});
