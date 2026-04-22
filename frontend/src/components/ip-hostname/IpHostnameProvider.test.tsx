import { act, render, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
  HOSTNAME_STREAM_URL,
  IpHostnameProvider,
  useIpHostnameContext,
} from "@/components/ip-hostname/IpHostnameProvider";
import { useIpHostname } from "@/components/ip-hostname/useIpHostname";

/**
 * Minimal in-memory EventSource stand-in. The real provider attaches a
 * listener for the `"hostname_resolved"` event (not `onmessage`), so the
 * mock mirrors that surface and nothing else.
 */
class MockEventSource {
  static instances: MockEventSource[] = [];
  listeners: Record<string, Array<(event: { data: string }) => void>> = {};
  readyState = 1;

  constructor(public url: string) {
    MockEventSource.instances.push(this);
  }

  addEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name] ?? [];
    list.push(handler);
    this.listeners[name] = list;
  }

  removeEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name];
    if (!list) return;
    const idx = list.indexOf(handler);
    if (idx >= 0) list.splice(idx, 1);
  }

  /** Dispatch a `hostname_resolved` frame with the given payload. */
  emit(payload: unknown): void {
    const data = JSON.stringify(payload);
    for (const handler of this.listeners.hostname_resolved ?? []) {
      handler({ data });
    }
  }

  /** Dispatch a malformed frame — tests the provider's parse-guard. */
  emitRaw(data: string): void {
    for (const handler of this.listeners.hostname_resolved ?? []) {
      handler({ data });
    }
  }

  close(): void {
    this.readyState = 2;
  }
}

function wrap() {
  return ({ children }: { children: ReactNode }) => (
    <IpHostnameProvider>{children}</IpHostnameProvider>
  );
}

beforeEach(() => {
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("IpHostnameProvider", () => {
  test(`opens exactly one EventSource to ${HOSTNAME_STREAM_URL} on mount`, () => {
    render(
      <IpHostnameProvider>
        <div />
      </IpHostnameProvider>,
    );
    expect(MockEventSource.instances).toHaveLength(1);
    expect(MockEventSource.instances[0]?.url).toBe(HOSTNAME_STREAM_URL);
  });

  test("closes the EventSource on unmount", () => {
    const { unmount } = render(
      <IpHostnameProvider>
        <div />
      </IpHostnameProvider>,
    );
    const source = MockEventSource.instances[0];
    expect(source?.readyState).toBe(1);
    unmount();
    expect(source?.readyState).toBe(2);
  });

  test("stores a positive hit from the SSE stream", () => {
    const { result } = renderHook(() => useIpHostname("10.0.0.1"), { wrapper: wrap() });
    expect(result.current).toBeUndefined();

    act(() => {
      MockEventSource.instances[0]?.emit({ ip: "10.0.0.1", hostname: "a.example.com" });
    });
    expect(result.current).toBe("a.example.com");
  });

  test("stores a negative hit as null (hostname omitted on the wire)", () => {
    const { result } = renderHook(() => useIpHostname("10.0.0.2"), { wrapper: wrap() });

    act(() => {
      MockEventSource.instances[0]?.emit({ ip: "10.0.0.2" });
    });
    expect(result.current).toBeNull();
  });

  test("stores a negative hit when hostname is explicitly null", () => {
    const { result } = renderHook(() => useIpHostname("10.0.0.3"), { wrapper: wrap() });

    act(() => {
      MockEventSource.instances[0]?.emit({ ip: "10.0.0.3", hostname: null });
    });
    expect(result.current).toBeNull();
  });

  test("handles IPv6 literals end-to-end", () => {
    const { result } = renderHook(() => useIpHostname("2001:db8::1"), { wrapper: wrap() });

    act(() => {
      MockEventSource.instances[0]?.emit({ ip: "2001:db8::1", hostname: "v6.example.com" });
    });
    expect(result.current).toBe("v6.example.com");
  });

  test("silently ignores malformed frames", () => {
    const { result } = renderHook(() => useIpHostname("10.0.0.4"), { wrapper: wrap() });

    act(() => {
      MockEventSource.instances[0]?.emitRaw("not json");
      MockEventSource.instances[0]?.emit({ ip: 42 });
      MockEventSource.instances[0]?.emit({ hostname: "no.ip.example.com" });
    });
    expect(result.current).toBeUndefined();
  });

  test("seedFromResponse primes the map for a synchronous render", () => {
    // Render hook inside the provider — call seedFromResponse, then observe
    // via useIpHostname in a child hook.
    const { result } = renderHook(
      () => {
        const ctx = useIpHostnameContext();
        const value = useIpHostname("10.0.0.5");
        return { ctx, value };
      },
      { wrapper: wrap() },
    );

    act(() => {
      result.current.ctx.seedFromResponse([
        { ip: "10.0.0.5", hostname: "seed.example.com" },
        { ip: "10.0.0.6", hostname: null }, // negative cache — stored as null
      ]);
    });

    expect(result.current.value).toBe("seed.example.com");
  });

  test("seedFromResponse ignores undefined hostnames so SSE values survive", () => {
    const { result } = renderHook(
      () => {
        const ctx = useIpHostnameContext();
        const value = useIpHostname("10.0.0.7");
        return { ctx, value };
      },
      { wrapper: wrap() },
    );

    // Stream a positive hit first.
    act(() => {
      MockEventSource.instances[0]?.emit({ ip: "10.0.0.7", hostname: "streamed.example.com" });
    });
    expect(result.current.value).toBe("streamed.example.com");

    // Now seed a cold-miss DTO (hostname absent → undefined). The provider
    // should NOT overwrite the streamed value.
    act(() => {
      result.current.ctx.seedFromResponse([{ ip: "10.0.0.7", hostname: undefined }]);
    });
    expect(result.current.value).toBe("streamed.example.com");
  });

  test("seedFromResponse is referentially stable across renders", () => {
    const seen: Array<(entries: Iterable<{ ip: string; hostname?: string | null }>) => void> = [];
    const { rerender } = renderHook(
      () => {
        const { seedFromResponse } = useIpHostnameContext();
        seen.push(seedFromResponse);
      },
      { wrapper: wrap() },
    );
    rerender();
    rerender();
    expect(seen.length).toBeGreaterThanOrEqual(3);
    expect(seen[0]).toBe(seen[1]);
    expect(seen[1]).toBe(seen[2]);
  });

  test("falls back gracefully when EventSource is missing", () => {
    vi.stubGlobal("EventSource", undefined);
    // Should not throw on mount; the seed path still works.
    const { result } = renderHook(
      () => {
        const ctx = useIpHostnameContext();
        const value = useIpHostname("10.0.0.8");
        return { ctx, value };
      },
      { wrapper: wrap() },
    );

    act(() => {
      result.current.ctx.seedFromResponse([{ ip: "10.0.0.8", hostname: "seed-only" }]);
    });
    expect(result.current.value).toBe("seed-only");
  });

  test("throws a descriptive error when the context is consumed outside the provider", () => {
    // Silence the React error-boundary warning for a clean test log.
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(() => renderHook(() => useIpHostnameContext())).toThrow(/IpHostnameProvider missing/);
    errSpy.mockRestore();
  });
});
