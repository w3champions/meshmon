/**
 * Read-only hooks for the `/history/pair` page.
 *
 * The three endpoints feed the picker + time-series view and are all regular
 * `useQuery`s:
 *
 * - `useHistorySources` → list of agents that have produced at least one
 *   measurement; cached with a 30s stale window since it barely moves.
 * - `useHistoryDestinations({ source, q })` → destinations seen from a given
 *   source, with optional catalogue/IP substring filter. Disabled until the
 *   caller has picked a source.
 * - `useHistoryMeasurements({ source, destination, protocols, from, to })` →
 *   raw measurement rows for the chart and MTR drilldown. Disabled until the
 *   caller has both source + destination. The server returns up to
 *   `HISTORY_MEASUREMENTS_CAP + 1` rows as a truncation probe; the page
 *   displays the first `HISTORY_MEASUREMENTS_CAP` and shows a cap notice when
 *   the extra row arrives.
 */

import { type UseQueryResult, useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import type { ProbeProtocol } from "@/api/hooks/campaigns";
import type { components } from "@/api/schema.gen";

export type HistorySource = components["schemas"]["HistorySourceDto"];
export type HistoryDestination = components["schemas"]["HistoryDestinationDto"];
export type HistoryMeasurement = components["schemas"]["HistoryMeasurementDto"];

export const HISTORY_SOURCES_KEY = ["history", "sources"] as const;

export function historyDestinationsKey(source: string, q: string | undefined) {
  return ["history", "destinations", source, q ?? ""] as const;
}

export function historyMeasurementsKey(
  source: string,
  destination: string,
  protocols: readonly ProbeProtocol[] | undefined,
  from: string | undefined,
  to: string | undefined,
) {
  return [
    "history",
    "measurements",
    source,
    destination,
    protocols ? [...protocols] : [],
    from ?? "",
    to ?? "",
  ] as const;
}

/**
 * Stale window for the source list. Agents churn slowly; 30s keeps the
 * picker from re-fetching on every mount without masking real membership
 * changes.
 */
const HISTORY_SOURCES_STALE_MS = 30_000;

export function useHistorySources(): UseQueryResult<HistorySource[], Error> {
  return useQuery({
    queryKey: HISTORY_SOURCES_KEY,
    staleTime: HISTORY_SOURCES_STALE_MS,
    queryFn: async (): Promise<HistorySource[]> => {
      const { data, error } = await api.GET("/api/history/sources");
      if (error) throw new Error("failed to fetch history sources", { cause: error });
      return (data ?? []) as HistorySource[];
    },
  });
}

export function useHistoryDestinations(
  source: string | undefined,
  q: string | undefined,
): UseQueryResult<HistoryDestination[], Error> {
  return useQuery({
    queryKey: historyDestinationsKey(source ?? "", q),
    enabled: !!source,
    queryFn: async (): Promise<HistoryDestination[]> => {
      // queryFn only runs when enabled → source is defined.
      const src = source as string;
      const { data, error } = await api.GET("/api/history/destinations", {
        params: {
          query: {
            source: src,
            ...(q ? { q } : {}),
          },
        },
      });
      if (error) throw new Error("failed to fetch history destinations", { cause: error });
      return (data ?? []) as HistoryDestination[];
    },
  });
}

export interface HistoryMeasurementsFilter {
  source: string;
  destination: string;
  /** Empty / undefined means "all protocols" (no `protocols` query param). */
  protocols?: readonly ProbeProtocol[];
  /** RFC 3339 inclusive lower bound. */
  from?: string;
  /** RFC 3339 inclusive upper bound. */
  to?: string;
}

export function useHistoryMeasurements(
  filter: HistoryMeasurementsFilter | null,
): UseQueryResult<HistoryMeasurement[], Error> {
  return useQuery({
    queryKey: filter
      ? historyMeasurementsKey(
          filter.source,
          filter.destination,
          filter.protocols,
          filter.from,
          filter.to,
        )
      : ["history", "measurements", "__disabled__"],
    enabled: !!filter,
    queryFn: async (): Promise<HistoryMeasurement[]> => {
      const f = filter as HistoryMeasurementsFilter;
      const { data, error } = await api.GET("/api/history/measurements", {
        params: {
          query: {
            source: f.source,
            destination: f.destination,
            ...(f.protocols && f.protocols.length > 0 ? { protocols: f.protocols.join(",") } : {}),
            ...(f.from ? { from: f.from } : {}),
            ...(f.to ? { to: f.to } : {}),
          },
        },
      });
      if (error) throw new Error("failed to fetch history measurements", { cause: error });
      return (data ?? []) as HistoryMeasurement[];
    },
  });
}
