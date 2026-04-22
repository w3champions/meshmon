import { act, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
  IpHostnameProvider,
  useIpHostnameContext,
} from "@/components/ip-hostname/IpHostnameProvider";
import { useIpHostname } from "@/components/ip-hostname/useIpHostname";

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

function wrap() {
  return ({ children }: { children: ReactNode }) => (
    <IpHostnameProvider>{children}</IpHostnameProvider>
  );
}

describe("useIpHostname", () => {
  test("returns undefined for cold miss", () => {
    const { result } = renderHook(() => useIpHostname("10.0.0.1"), { wrapper: wrap() });
    expect(result.current).toBeUndefined();
  });

  test("returns the hostname string when seeded positive", () => {
    const { result } = renderHook(
      () => {
        const ctx = useIpHostnameContext();
        const v = useIpHostname("10.0.0.1");
        return { ctx, v };
      },
      { wrapper: wrap() },
    );
    act(() => {
      result.current.ctx.seedFromResponse([{ ip: "10.0.0.1", hostname: "a.example.com" }]);
    });
    expect(result.current.v).toBe("a.example.com");
  });

  test("returns null when seeded negative", () => {
    const { result } = renderHook(
      () => {
        const ctx = useIpHostnameContext();
        const v = useIpHostname("10.0.0.2");
        return { ctx, v };
      },
      { wrapper: wrap() },
    );
    act(() => {
      result.current.ctx.seedFromResponse([{ ip: "10.0.0.2", hostname: null }]);
    });
    expect(result.current.v).toBeNull();
  });
});
