import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  CAMPAIGN_PREVIEW_KEY,
  CAMPAIGNS_LIST_KEY,
  campaignKey,
  campaignMeasurementsPrefixKey,
  campaignPairsKey,
  campaignPreviewKey,
} from "@/api/hooks/campaigns";

/** Minimal in-memory EventSource stand-in for deterministic tests. */
class MockEventSource {
  static instances: MockEventSource[] = [];
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: ((event?: unknown) => void) | null = null;
  onopen: ((event?: unknown) => void) | null = null;
  readyState = 1;

  constructor(public url: string) {
    MockEventSource.instances.push(this);
  }

  emit(payload: unknown): void {
    this.onmessage?.({ data: JSON.stringify(payload) });
  }

  raise(): void {
    this.onerror?.();
  }

  close(): void {
    this.readyState = 2;
  }
}

const CAMPAIGN_ID = "11111111-1111-1111-1111-111111111111";

function makeQueryClient(): QueryClient {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

function wrapWith(qc: QueryClient) {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

beforeEach(() => {
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useCampaignStream", () => {
  test("opens /api/campaigns/stream on mount", () => {
    const qc = makeQueryClient();
    renderHook(() => useCampaignStream(), { wrapper: wrapWith(qc) });
    expect(MockEventSource.instances).toHaveLength(1);
    expect(MockEventSource.instances[0]?.url).toBe("/api/campaigns/stream");
  });

  test("invalidates list, entry, and preview on `state_changed`", () => {
    const qc = makeQueryClient();
    qc.setQueryData([...CAMPAIGNS_LIST_KEY, {}], []);
    qc.setQueryData(campaignKey(CAMPAIGN_ID), {});
    qc.setQueryData(campaignPreviewKey(CAMPAIGN_ID), {});

    renderHook(() => useCampaignStream(), { wrapper: wrapWith(qc) });
    act(() => {
      MockEventSource.instances[0]?.emit({
        kind: "state_changed",
        campaign_id: CAMPAIGN_ID,
        state: "running",
      });
    });

    expect(qc.getQueryState([...CAMPAIGNS_LIST_KEY, {}])?.isInvalidated).toBe(true);
    expect(qc.getQueryState(campaignKey(CAMPAIGN_ID))?.isInvalidated).toBe(true);
    expect(qc.getQueryState(campaignPreviewKey(CAMPAIGN_ID))?.isInvalidated).toBe(true);
  });

  test("invalidates entry, pairs, preview, and measurements prefix on `pair_settled`", () => {
    const qc = makeQueryClient();
    qc.setQueryData(campaignKey(CAMPAIGN_ID), {});
    qc.setQueryData(campaignPairsKey(CAMPAIGN_ID), []);
    qc.setQueryData(campaignPreviewKey(CAMPAIGN_ID), {});
    // Prime a measurements cache entry with an arbitrary filter so we can
    // verify the prefix sweep reaches it.
    const measurementsFilterKey = [
      ...campaignMeasurementsPrefixKey(CAMPAIGN_ID),
      { protocol: "icmp" },
    ];
    qc.setQueryData(measurementsFilterKey, { entries: [] });

    renderHook(() => useCampaignStream(), { wrapper: wrapWith(qc) });
    act(() => {
      MockEventSource.instances[0]?.emit({
        kind: "pair_settled",
        campaign_id: CAMPAIGN_ID,
      });
    });

    expect(qc.getQueryState(campaignKey(CAMPAIGN_ID))?.isInvalidated).toBe(true);
    expect(qc.getQueryState(campaignPairsKey(CAMPAIGN_ID))?.isInvalidated).toBe(true);
    expect(qc.getQueryState(campaignPreviewKey(CAMPAIGN_ID))?.isInvalidated).toBe(true);
    expect(qc.getQueryState(measurementsFilterKey)?.isInvalidated).toBe(true);
  });

  test("invalidates list + all previews on `lag` and emits a warning", () => {
    const qc = makeQueryClient();
    qc.setQueryData([...CAMPAIGNS_LIST_KEY, {}], []);
    qc.setQueryData(campaignPreviewKey(CAMPAIGN_ID), {});
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    renderHook(() => useCampaignStream(), { wrapper: wrapWith(qc) });
    act(() => {
      MockEventSource.instances[0]?.emit({ kind: "lag", missed: 7 });
    });

    expect(qc.getQueryState([...CAMPAIGNS_LIST_KEY, {}])?.isInvalidated).toBe(true);
    // The prefix `CAMPAIGN_PREVIEW_KEY` sweeps every cached preview entry; the
    // single-row preview we primed above should flip to invalidated.
    expect(qc.getQueryState(campaignPreviewKey(CAMPAIGN_ID))?.isInvalidated).toBe(true);
    const invalidatedKeys = invalidateSpy.mock.calls.map((c) => c[0]?.queryKey);
    expect(invalidatedKeys).toContainEqual(CAMPAIGNS_LIST_KEY);
    expect(invalidatedKeys).toContainEqual(CAMPAIGN_PREVIEW_KEY);
    expect(warnSpy).toHaveBeenCalled();
  });

  test("reconnects with capped exponential backoff on error", () => {
    vi.useFakeTimers();
    const qc = makeQueryClient();
    renderHook(() => useCampaignStream(), { wrapper: wrapWith(qc) });
    expect(MockEventSource.instances).toHaveLength(1);

    // Trigger an error — no reconnect yet, a timer is scheduled for 1s.
    act(() => {
      MockEventSource.instances[0]?.raise();
    });
    expect(MockEventSource.instances).toHaveLength(1);

    // First delay is 1s.
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(MockEventSource.instances).toHaveLength(2);

    // Fail again → second delay is 2s.
    act(() => {
      MockEventSource.instances[1]?.raise();
    });
    act(() => {
      vi.advanceTimersByTime(1999);
    });
    expect(MockEventSource.instances).toHaveLength(2);
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(MockEventSource.instances).toHaveLength(3);
  });

  test("cleans up EventSource and pending reconnect timer on unmount", () => {
    vi.useFakeTimers();
    const qc = makeQueryClient();
    const { unmount } = renderHook(() => useCampaignStream(), { wrapper: wrapWith(qc) });
    const first = MockEventSource.instances[0];
    expect(first).toBeDefined();
    act(() => {
      first?.raise();
    });
    unmount();
    expect(first?.readyState).toBe(2);
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    // No new EventSource after unmount.
    expect(MockEventSource.instances).toHaveLength(1);
  });
});
