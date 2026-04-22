import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { useRefreshHostname } from "@/components/ip-hostname/useRefreshHostname";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useRefreshHostname", () => {
  test("POSTs to /api/hostnames/:ip/refresh and resolves on 202", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(null, { status: 202 }));

    const { result } = renderHook(() => useRefreshHostname());
    await expect(result.current("10.0.0.1")).resolves.toBeUndefined();

    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [url, init] = fetchSpy.mock.calls[0] ?? [];
    expect(url).toBe("/api/hostnames/10.0.0.1/refresh");
    expect(init?.method).toBe("POST");
    expect(init?.credentials).toBe("include");
  });

  test("percent-encodes IPv6 literals so colons survive the URL boundary", async () => {
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(null, { status: 202 }));

    const { result } = renderHook(() => useRefreshHostname());
    await result.current("2001:db8::1");

    const [url] = fetchSpy.mock.calls[0] ?? [];
    expect(url).toBe("/api/hostnames/2001%3Adb8%3A%3A1/refresh");
  });

  test("rejects on non-2xx status so callers can toast", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(null, { status: 429 }));

    const { result } = renderHook(() => useRefreshHostname());
    await expect(result.current("10.0.0.1")).rejects.toThrow(/HTTP 429/);
  });

  test("returns a stable function identity across renders", () => {
    const { result, rerender } = renderHook(() => useRefreshHostname());
    const first = result.current;
    rerender();
    expect(result.current).toBe(first);
  });
});
