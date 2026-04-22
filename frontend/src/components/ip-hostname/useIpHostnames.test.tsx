import { act, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
  IpHostnameProvider,
  useIpHostnameContext,
} from "@/components/ip-hostname/IpHostnameProvider";
import { useIpHostnames } from "@/components/ip-hostname/useIpHostnames";

class MockEventSource {
  static instances: MockEventSource[] = [];
  listeners: Record<string, Array<(event: { data: string }) => void>> = {};
  constructor(public url: string) {
    MockEventSource.instances.push(this);
  }
  addEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name] ?? [];
    list.push(handler);
    this.listeners[name] = list;
  }
  removeEventListener(): void {}
  emit(payload: unknown): void {
    for (const h of this.listeners.hostname_resolved ?? []) {
      h({ data: JSON.stringify(payload) });
    }
  }
  close(): void {}
}

beforeEach(() => {
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

function wrap() {
  return ({ children }: { children: ReactNode }) => (
    <IpHostnameProvider>{children}</IpHostnameProvider>
  );
}

describe("useIpHostnames", () => {
  test("returns a map with cold/positive/negative entries", () => {
    const { result } = renderHook(
      () => {
        const ctx = useIpHostnameContext();
        const map = useIpHostnames(["10.0.0.1", "10.0.0.2", "10.0.0.3"]);
        return { ctx, map };
      },
      { wrapper: wrap() },
    );

    act(() => {
      result.current.ctx.seedFromResponse([
        { ip: "10.0.0.1", hostname: "a.example.com" }, // positive
        { ip: "10.0.0.2", hostname: null }, //             negative
        // 10.0.0.3 not seeded — cold miss
      ]);
    });

    expect(result.current.map.get("10.0.0.1")).toBe("a.example.com");
    expect(result.current.map.get("10.0.0.2")).toBeNull();
    expect(result.current.map.get("10.0.0.3")).toBeUndefined();
  });

  test("keeps a stable map identity when no relevant IP changes", () => {
    const { result, rerender } = renderHook(
      ({ ips }: { ips: string[] }) => {
        const ctx = useIpHostnameContext();
        const map = useIpHostnames(ips);
        return { ctx, map };
      },
      { wrapper: wrap(), initialProps: { ips: ["10.0.0.1", "10.0.0.2"] } },
    );

    act(() => {
      result.current.ctx.seedFromResponse([{ ip: "10.0.0.1", hostname: "a.example.com" }]);
    });
    const before = result.current.map;

    // Stream an update for an IP the caller DOESN'T track. The bulk hook
    // must return the same map reference so downstream `useMemo` effects
    // don't fire on unrelated changes.
    act(() => {
      MockEventSource.instances[0]?.emit({ ip: "192.0.2.99", hostname: "other.example.com" });
    });

    // Pass a new-identity array with the same contents — hook must still
    // return the same map reference.
    rerender({ ips: ["10.0.0.1", "10.0.0.2"] });
    expect(result.current.map).toBe(before);
  });

  test("returns a fresh map identity when a tracked IP changes", () => {
    const { result } = renderHook(
      () => {
        const ctx = useIpHostnameContext();
        const map = useIpHostnames(["10.0.0.1", "10.0.0.2"]);
        return { ctx, map };
      },
      { wrapper: wrap() },
    );

    const before = result.current.map;

    act(() => {
      MockEventSource.instances[0]?.emit({ ip: "10.0.0.1", hostname: "new.example.com" });
    });

    expect(result.current.map).not.toBe(before);
    expect(result.current.map.get("10.0.0.1")).toBe("new.example.com");
  });

  test("deduplicates + sorts the input IP set", () => {
    const { result } = renderHook(() => useIpHostnames(["10.0.0.2", "10.0.0.1", "10.0.0.1"]), {
      wrapper: wrap(),
    });
    // Two unique IPs, not three.
    expect(Array.from(result.current.keys())).toEqual(["10.0.0.1", "10.0.0.2"]);
  });
});
