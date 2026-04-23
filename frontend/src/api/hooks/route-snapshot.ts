import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import type { components } from "@/api/schema.gen";
import { useSeedHostnamesOnResponse } from "@/components/ip-hostname";

export type RouteSnapshotDetail = components["schemas"]["RouteSnapshotDetail"];

export interface UseRouteSnapshotOpts {
  source: string;
  target: string;
  id: number | undefined;
}

export function useRouteSnapshot(opts: UseRouteSnapshotOpts) {
  const { source, target, id } = opts;
  const query = useQuery({
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
  // Seed the shared hostname map from every response. Seeds hop-level IPs
  // from `RouteSnapshotDetail.hops[*].observed_ips[*]`. Null (404 / not-found)
  // produces no entries — the guard in `useSeedHostnamesOnResponse` handles it.
  useSeedHostnamesOnResponse(query.data, function* (snap) {
    if (!snap) return;
    for (const hop of snap.hops) {
      for (const o of hop.observed_ips) {
        yield { ip: o.ip, hostname: o.hostname };
      }
    }
  });
  return query;
}
