import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type RouteSnapshotSummary = components["schemas"]["RouteSnapshotSummary"];

export function useRecentRouteChanges(limit: number = 10) {
  return useQuery({
    queryKey: ["recent-routes", limit],
    queryFn: async () => {
      const { data, error } = await api.GET("/api/routes/recent", {
        params: { query: { limit } },
      });
      if (error) throw new Error("failed to fetch recent route changes");
      if (!data) throw new Error("empty response");
      return data;
    },
    refetchInterval: 30_000,
  });
}
