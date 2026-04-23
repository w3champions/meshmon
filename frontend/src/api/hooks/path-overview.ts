import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import type { components } from "@/api/schema.gen";
import { useSeedHostnamesOnResponse } from "@/components/ip-hostname";
import type { TimeRangeKey } from "@/lib/time-range";
import { rangeBounds } from "@/lib/time-range";

export type PathOverviewResponse = components["schemas"]["PathOverviewResponse"];

export interface UsePathOverviewOpts {
  source: string;
  target: string;
  range: TimeRangeKey;
  customFrom?: string;
  customTo?: string;
  protocol?: "icmp" | "udp" | "tcp";
}

/**
 * Stabilize `now` to the current minute so the queryKey does not churn on
 * every render. The 60_000ms refetchInterval drives the rolling-window
 * advance — see plan T19 Task 10.
 */
function currentMinuteIso(): string {
  const now = new Date();
  now.setSeconds(0, 0);
  return now.toISOString();
}

export function usePathOverview(opts: UsePathOverviewOpts) {
  const { source, target, range, customFrom, customTo, protocol } = opts;

  let fromIso: string;
  let toIso: string;
  if (range === "custom") {
    if (!customFrom || !customTo) {
      throw new Error("custom range requires customFrom and customTo");
    }
    fromIso = new Date(customFrom).toISOString();
    toIso = new Date(customTo).toISOString();
  } else {
    const nowIso = currentMinuteIso();
    const { from, to } = rangeBounds(range, undefined, new Date(nowIso));
    fromIso = from.toISOString();
    toIso = to.toISOString();
  }

  const query = useQuery({
    queryKey: ["path-overview", source, target, fromIso, toIso, protocol],
    queryFn: async (): Promise<PathOverviewResponse> => {
      const { data, error } = await api.GET("/api/paths/{src}/{tgt}/overview", {
        params: {
          path: { src: source, tgt: target },
          query: { from: fromIso, to: toIso, protocol },
        },
      });
      if (error) throw new Error("failed to fetch path overview", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
    // Keep the prior result rendered while a new key fetches — but only when
    // the underlying (source, target) pair is unchanged. Protocol/range changes
    // on the same path should not flash a skeleton, while navigating to a
    // different pair must fall back to a loading state rather than briefly
    // rendering the previous path's data against the new URL.
    placeholderData: (previous, previousQuery) => {
      if (!previous || !previousQuery) return undefined;
      const [, prevSource, prevTarget] = previousQuery.queryKey as [
        string,
        string,
        string,
        ...unknown[],
      ];
      return prevSource === source && prevTarget === target ? previous : undefined;
    },
    refetchInterval: 60_000,
  });

  // Seed the shared hostname map from every response. Seeds source + target
  // agent IPs and all hop IPs from the three protocol snapshots (icmp/udp/tcp)
  // in `latest_by_protocol`. Each field may be null/undefined — guard before
  // iterating.
  useSeedHostnamesOnResponse(query.data, function* (d) {
    yield { ip: d.source.ip, hostname: d.source.hostname };
    yield { ip: d.target.ip, hostname: d.target.hostname };
    for (const proto of ["icmp", "udp", "tcp"] as const) {
      const snap = d.latest_by_protocol[proto];
      if (!snap) continue;
      for (const hop of snap.hops) {
        for (const o of hop.observed_ips) {
          yield { ip: o.ip, hostname: o.hostname };
        }
      }
    }
  });

  return query;
}
