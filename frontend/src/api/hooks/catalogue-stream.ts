import { type QueryClient, useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import type { components } from "@/api/schema.gen";

export type CatalogueEntry = components["schemas"]["CatalogueEntryDto"];

/**
 * Catalogue stream event shapes.
 *
 * The generated `components["schemas"]["CatalogueEvent"]` covers the four
 * domain-level variants (`created`, `updated`, `deleted`, `enrichment_progress`)
 * that are emitted via the typed broker. The SSE handler ALSO emits a synthetic
 * `{"kind":"lag","missed":N}` frame when the broadcast buffer overflows —
 * that shape bypasses the utoipa-derived enum, so we augment it locally
 * rather than forking the generated schema.
 */
type CatalogueEventSchema = components["schemas"]["CatalogueEvent"];
type LagFrame = { kind: "lag"; missed: number };
type CatalogueStreamEvent = CatalogueEventSchema | LagFrame;

const CATALOGUE_LIST_KEY = ["catalogue", "list"] as const;
const CATALOGUE_FACETS_KEY = ["catalogue", "facets"] as const;

function catalogueEntryKey(id: string) {
  return ["catalogue", "entry", id] as const;
}

/** Reconnect backoff schedule: 1s → 2s → 4s → 8s → 16s → 30s (cap). */
const INITIAL_BACKOFF_MS = 1_000;
const MAX_BACKOFF_MS = 30_000;

function nextBackoff(current: number): number {
  return Math.min(current * 2, MAX_BACKOFF_MS);
}

function isCatalogueStreamEvent(value: unknown): value is CatalogueStreamEvent {
  if (typeof value !== "object" || value === null) return false;
  const kind = (value as { kind?: unknown }).kind;
  return (
    kind === "created" ||
    kind === "updated" ||
    kind === "deleted" ||
    kind === "enrichment_progress" ||
    kind === "lag"
  );
}

function applyEvent(queryClient: QueryClient, event: CatalogueStreamEvent): void {
  switch (event.kind) {
    case "created": {
      // New rows shift the current list window and facet counts. Don't
      // touch individual entry keys — they don't exist yet for a fresh
      // row, and the list refetch carries the new entry naturally.
      queryClient.invalidateQueries({ queryKey: CATALOGUE_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
      return;
    }
    case "updated": {
      // The single entry cache is stale. Field changes can also change
      // sort order or filter membership inside the current list window,
      // so we invalidate the list prefix too. Facets may shift when an
      // operator corrects geo/ASN fields — invalidate conservatively.
      queryClient.invalidateQueries({ queryKey: catalogueEntryKey(event.id) });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
      return;
    }
    case "enrichment_progress": {
      // Patch in place so status chips animate without a round-trip.
      // If the entry isn't in the cache, skip — nothing to update.
      const key = catalogueEntryKey(event.id);
      const existing = queryClient.getQueryData<CatalogueEntry>(key);
      if (existing) {
        queryClient.setQueryData<CatalogueEntry>(key, {
          ...existing,
          enrichment_status: event.status,
        });
      }
      // `enrichment_status` drives a facet bucket — counts change.
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
      return;
    }
    case "deleted": {
      queryClient.removeQueries({ queryKey: catalogueEntryKey(event.id) });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
      return;
    }
    case "lag": {
      // Buffer overflow — our cached view may be missing events. Force a
      // refetch of the list and facets; individual entries can stay
      // whatever they are until the user navigates.
      console.warn(`[catalogue-stream] missed ${event.missed} event(s); forcing list refetch`);
      queryClient.invalidateQueries({ queryKey: CATALOGUE_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
      return;
    }
  }
}

/**
 * Subscribe to catalogue SSE events and reconcile the query cache.
 *
 * Fire-and-forget: mount this hook once at the page level. It opens
 * `/api/catalogue/stream`, patches the TanStack-Query cache on each event,
 * and reconnects with capped exponential backoff on transport errors.
 */
export function useCatalogueStream(): void {
  const queryClient = useQueryClient();

  useEffect(() => {
    let source: EventSource | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    let backoffMs = INITIAL_BACKOFF_MS;
    let disposed = false;

    const connect = (): void => {
      if (disposed) return;
      source = new EventSource("/api/catalogue/stream");
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
          console.warn("[catalogue-stream] malformed frame", error);
          return;
        }
        if (!isCatalogueStreamEvent(parsed)) {
          console.warn("[catalogue-stream] unknown event shape", parsed);
          return;
        }
        applyEvent(queryClient, parsed);
      };
      source.onerror = () => {
        if (disposed) return;
        source?.close();
        source = null;
        const delay = backoffMs;
        backoffMs = nextBackoff(backoffMs);
        console.warn(`[catalogue-stream] connection error; reconnecting in ${delay}ms`);
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
