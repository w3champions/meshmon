import { act, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { IpHostnameProvider } from "@/components/ip-hostname/IpHostnameProvider";
import { useIpHostname } from "@/components/ip-hostname/useIpHostname";
import { useSeedHostnamesOnResponse } from "@/components/ip-hostname/useSeedHostnamesOnResponse";

class NoopEventSource {
  static instances: NoopEventSource[] = [];
  constructor(public url: string) {
    NoopEventSource.instances.push(this);
  }
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

beforeEach(() => {
  NoopEventSource.instances = [];
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

interface SampleDto {
  entries: Array<{ ip: string; hostname?: string | null }>;
}

describe("useSeedHostnamesOnResponse", () => {
  test("seeds the provider when data arrives", () => {
    const { result, rerender } = renderHook(
      ({ data }: { data: SampleDto | undefined }) => {
        useSeedHostnamesOnResponse(data, (d) => d.entries);
        return useIpHostname("10.0.0.1");
      },
      { wrapper: wrap(), initialProps: { data: undefined as SampleDto | undefined } },
    );

    // Before data arrives: cold miss.
    expect(result.current).toBeUndefined();

    // Simulate query resolution.
    act(() => {
      rerender({ data: { entries: [{ ip: "10.0.0.1", hostname: "seeded.example.com" }] } });
    });
    expect(result.current).toBe("seeded.example.com");
  });

  test("no-ops when data is undefined", () => {
    // The seed effect must simply not fire for an un-resolved query; the
    // assertion is the absence of a throw from the provider.
    const { result } = renderHook(
      () => {
        useSeedHostnamesOnResponse<SampleDto>(undefined, (d) => d.entries);
        return useIpHostname("10.0.0.2");
      },
      { wrapper: wrap() },
    );
    expect(result.current).toBeUndefined();
  });

  test("handles iterable selectors that aren't arrays", () => {
    // Query hooks often return Maps or generators (e.g. flattened pages).
    // The hook spec accepts any Iterable<{ ip, hostname }>; cover that
    // path so a future infinite-query wiring doesn't have to pre-materialise.
    function* entries() {
      yield { ip: "10.0.0.3", hostname: "gen.example.com" };
      yield { ip: "10.0.0.4", hostname: null };
    }
    const { result, rerender } = renderHook(
      ({ tick }: { tick: number }) => {
        useSeedHostnamesOnResponse(tick > 0 ? { entries } : undefined, (d) => d.entries());
        return useIpHostname("10.0.0.3");
      },
      { wrapper: wrap(), initialProps: { tick: 0 } },
    );
    expect(result.current).toBeUndefined();

    act(() => {
      rerender({ tick: 1 });
    });
    expect(result.current).toBe("gen.example.com");
  });
});
