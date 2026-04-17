import { useQuery } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type RouteSnapshotSummary = components["schemas"]["RouteSnapshotSummary"];

export interface UseNearbySnapshotsOpts {
  source: string;
  target: string;
  protocol: string;
  aroundTimeMs: number;
}

export interface NearbySnapshotsResult {
  snapshots: RouteSnapshotSummary[];
  halfWindowMs: number;
  findClosest(targetMs: number): RouteSnapshotSummary | undefined;
  getNeighbors(currentId: number): {
    prev?: RouteSnapshotSummary;
    next?: RouteSnapshotSummary;
  };
  isLoading: boolean;
  isError: boolean;
}

const MS_MIN = 60 * 1_000;
const MS_HOUR = 60 * MS_MIN;
const INITIAL_HALF_WINDOW_MS = 15 * MS_MIN;
const MAX_HALF_WINDOW_MS = 24 * MS_HOUR;
const NEIGHBORS_PER_SIDE_TARGET = 3;

export function useNearbySnapshots(opts: UseNearbySnapshotsOpts): NearbySnapshotsResult {
  const { source, target, protocol, aroundTimeMs } = opts;
  const [halfWindowMs, setHalfWindowMs] = useState(INITIAL_HALF_WINDOW_MS);

  // Reset widen state only when the (source, target, protocol) triple changes
  // — a new mesh path has no reason to inherit a previously-widened window.
  // Moving aroundTimeMs within the same path keeps the widened state so a
  // dense area stays dense; a follow-up could add outside-of-span detection.
  // biome-ignore lint/correctness/useExhaustiveDependencies: intentional reset on path change only
  useEffect(() => {
    setHalfWindowMs(INITIAL_HALF_WINDOW_MS);
  }, [source, target, protocol]);

  const fromIso = new Date(aroundTimeMs - halfWindowMs).toISOString();
  const toIso = new Date(aroundTimeMs + halfWindowMs).toISOString();

  const query = useQuery({
    queryKey: ["nearby-snapshots", source, target, protocol, fromIso, toIso],
    queryFn: async () => {
      const { data, error } = await api.GET("/api/paths/{src}/{tgt}/routes", {
        params: {
          path: { src: source, tgt: target },
          query: { from: fromIso, to: toIso, protocol, limit: 500 },
        },
      });
      if (error) throw new Error("failed to fetch nearby snapshots", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
  });

  const snapshots = useMemo(() => {
    const items = query.data?.items ?? [];
    return [...items].sort((a, b) => Date.parse(a.observed_at) - Date.parse(b.observed_at));
  }, [query.data]);

  // Adaptive widening: if either side of the anchor has fewer than N
  // neighbors, double the window (capped).
  useEffect(() => {
    if (!query.data) return;
    if (halfWindowMs >= MAX_HALF_WINDOW_MS) return;
    const below = snapshots.filter((s) => Date.parse(s.observed_at) < aroundTimeMs).length;
    const above = snapshots.filter((s) => Date.parse(s.observed_at) > aroundTimeMs).length;
    if (below < NEIGHBORS_PER_SIDE_TARGET || above < NEIGHBORS_PER_SIDE_TARGET) {
      const next = Math.min(halfWindowMs * 2, MAX_HALF_WINDOW_MS);
      if (next !== halfWindowMs) setHalfWindowMs(next);
    }
  }, [query.data, snapshots, aroundTimeMs, halfWindowMs]);

  const findClosest = useCallback(
    (targetMs: number): RouteSnapshotSummary | undefined => {
      if (snapshots.length === 0) return undefined;
      let best = snapshots[0];
      let bestDelta = Math.abs(Date.parse(best.observed_at) - targetMs);
      for (const s of snapshots) {
        const d = Math.abs(Date.parse(s.observed_at) - targetMs);
        if (d < bestDelta) {
          best = s;
          bestDelta = d;
        }
      }
      return best;
    },
    [snapshots],
  );

  const getNeighbors = useCallback(
    (currentId: number): { prev?: RouteSnapshotSummary; next?: RouteSnapshotSummary } => {
      const idx = snapshots.findIndex((s) => s.id === currentId);
      if (idx < 0) return {};
      return {
        prev: idx > 0 ? snapshots[idx - 1] : undefined,
        next: idx < snapshots.length - 1 ? snapshots[idx + 1] : undefined,
      };
    },
    [snapshots],
  );

  return useMemo(
    () => ({
      snapshots,
      halfWindowMs,
      findClosest,
      getNeighbors,
      isLoading: query.isLoading,
      isError: query.isError,
    }),
    [snapshots, halfWindowMs, findClosest, getNeighbors, query.isLoading, query.isError],
  );
}
