import { type QueryClient, useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import {
  CAMPAIGN_PREVIEW_KEY,
  CAMPAIGNS_LIST_KEY,
  campaignEvaluationKey,
  campaignKey,
  campaignMeasurementsPrefixKey,
  campaignPairsKey,
  campaignPreviewKey,
} from "@/api/hooks/campaigns";
import type { components } from "@/api/schema.gen";

/**
 * Campaign stream event shapes.
 *
 * The generated `components["schemas"]["CampaignStreamEvent"]` covers the
 * domain-level variants (`state_changed`, `pair_settled`, `evaluated`) emitted
 * via the typed broker. The SSE handler ALSO emits a synthetic
 * `{"kind":"lag","missed":N}` frame when the broadcast buffer overflows — that
 * shape bypasses the utoipa-derived enum, so we augment it locally rather than
 * forking the generated schema.
 */
type CampaignStreamBase = components["schemas"]["CampaignStreamEvent"];
type LagFrame = { kind: "lag"; missed: number };
type CampaignStream = CampaignStreamBase | LagFrame;

/** Reconnect backoff schedule: 1s → 2s → 4s → 8s → 16s → 30s (cap). */
const INITIAL_BACKOFF_MS = 1_000;
const MAX_BACKOFF_MS = 30_000;

function nextBackoff(current: number): number {
  return Math.min(current * 2, MAX_BACKOFF_MS);
}

function isCampaignStreamEvent(value: unknown): value is CampaignStream {
  if (typeof value !== "object" || value === null) return false;
  const kind = (value as { kind?: unknown }).kind;
  return (
    kind === "state_changed" || kind === "pair_settled" || kind === "evaluated" || kind === "lag"
  );
}

function applyEvent(queryClient: QueryClient, event: CampaignStream): void {
  switch (event.kind) {
    case "state_changed": {
      // Lifecycle transition changes the list ordering (started_at / stopped_at
      // move), the single-row shell, and the dispatch preview (running /
      // completed campaigns report different `fresh` / `reusable` counts).
      queryClient.invalidateQueries({ queryKey: CAMPAIGNS_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: campaignKey(event.campaign_id) });
      queryClient.invalidateQueries({ queryKey: campaignPreviewKey(event.campaign_id) });
      return;
    }
    case "pair_settled": {
      // A pair transitioned to a terminal state. `pair_counts` on the campaign
      // shell shifts, the paginated pair list changes membership, and the
      // preview dispatch counts move as resolved pairs exit the `fresh`
      // bucket. The Raw tab's `/measurements` feed also gains a new row, so
      // invalidate the prefix so every cached filter variant refetches. Don't
      // touch the list key — the shell shape stays put.
      queryClient.invalidateQueries({ queryKey: campaignKey(event.campaign_id) });
      queryClient.invalidateQueries({ queryKey: campaignPairsKey(event.campaign_id) });
      queryClient.invalidateQueries({ queryKey: campaignPreviewKey(event.campaign_id) });
      queryClient.invalidateQueries({
        queryKey: campaignMeasurementsPrefixKey(event.campaign_id),
      });
      return;
    }
    case "evaluated": {
      // Evaluator rewrote the `campaign_evaluations` row and transitioned the
      // campaign into the `evaluated` state. Invalidate the evaluation read
      // (key may have no subscriber until the eval hook ships in T49 — safe
      // no-op in that case), the campaign shell (state / evaluated_at move),
      // and the list (sort order depends on evaluated_at).
      //
      // Load-bearing: `campaignEvaluationCandidatePairsKey` (the
      // drilldown dialog's paginated pair-detail feed) prepends
      // `campaignEvaluationKey(id)`, so this prefix invalidation
      // cascades to every cached drilldown variant. Do not narrow the
      // key here without updating `evaluation-pairs.ts` to add its
      // own SSE branch.
      queryClient.invalidateQueries({ queryKey: campaignEvaluationKey(event.campaign_id) });
      queryClient.invalidateQueries({ queryKey: campaignKey(event.campaign_id) });
      queryClient.invalidateQueries({ queryKey: CAMPAIGNS_LIST_KEY });
      return;
    }
    case "lag": {
      // Buffer overflow — our cached view may be missing events. We don't know
      // which campaign(s) were affected, so we sweep both the list and every
      // cached preview with the prefix key. Individual entry caches stay as
      // they are until the user navigates back to them (an SSE resubscribe on
      // focus is the right tool for that, not a global invalidate here).
      console.warn(`[campaign-stream] missed ${event.missed} event(s); forcing list refetch`);
      queryClient.invalidateQueries({ queryKey: CAMPAIGNS_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CAMPAIGN_PREVIEW_KEY });
      return;
    }
  }
}

/**
 * Subscribe to campaign SSE events and reconcile the query cache.
 *
 * Fire-and-forget: mount this hook once at the page level. It opens
 * `/api/campaigns/stream`, patches the TanStack-Query cache on each event,
 * and reconnects with capped exponential backoff on transport errors.
 */
export function useCampaignStream(): void {
  const queryClient = useQueryClient();

  useEffect(() => {
    let source: EventSource | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    let backoffMs = INITIAL_BACKOFF_MS;
    let disposed = false;

    const connect = (): void => {
      if (disposed) return;
      source = new EventSource("/api/campaigns/stream");
      source.onopen = () => {
        // Reset backoff on successful reconnect so a later transient
        // failure starts over at 1s rather than riding the previous cap.
        backoffMs = INITIAL_BACKOFF_MS;
      };
      source.onmessage = (event: MessageEvent<string>) => {
        let parsed: unknown;
        try {
          parsed = JSON.parse(event.data);
        } catch (error) {
          console.warn("[campaign-stream] malformed frame", error);
          return;
        }
        if (!isCampaignStreamEvent(parsed)) {
          console.warn("[campaign-stream] unknown event shape", parsed);
          return;
        }
        applyEvent(queryClient, parsed);
      };
      source.onerror = () => {
        if (disposed) return;
        source?.close();
        source = null;
        // Some browsers fire `onerror` more than once per dead connection.
        // Bail if a reconnect is already scheduled so we don't double-book
        // timers and end up with two concurrent EventSource instances.
        if (reconnectTimer !== null) return;
        const delay = backoffMs;
        backoffMs = nextBackoff(backoffMs);
        console.warn(`[campaign-stream] connection error; reconnecting in ${delay}ms`);
        reconnectTimer = setTimeout(() => {
          reconnectTimer = null;
          connect();
        }, delay);
      };
    };

    connect();

    return () => {
      disposed = true;
      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      source?.close();
      source = null;
    };
  }, [queryClient]);
}
