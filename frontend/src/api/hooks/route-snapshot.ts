import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type RouteSnapshotDetail = components["schemas"]["RouteSnapshotDetail"];

export interface UseRouteSnapshotOpts {
  source: string;
  target: string;
  id: number | undefined;
}

export function useRouteSnapshot(opts: UseRouteSnapshotOpts) {
  const { source, target, id } = opts;
  return useQuery({
    queryKey: ["route-snapshot", source, target, id],
    enabled: id !== undefined,
    queryFn: async (): Promise<RouteSnapshotDetail | null> => {
      const { data, error, response } = await api.GET(
        "/api/paths/{src}/{tgt}/routes/{snapshot_id}",
        {
          params: {
            path: { src: source, tgt: target, snapshot_id: id as number },
          },
        },
      );
      if (response?.status === 404) return null;
      if (error) throw new Error("failed to fetch snapshot", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
  });
}
