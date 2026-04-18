import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import type { components } from "@/api/schema.gen";
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

  return useQuery({
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
    // Keep the prior result rendered while a new key fetches. Without this,
    // every protocol/range change flips `isPending` on → PathDetail returns a
    // top-level skeleton → the whole page visually "refreshes". With
    // `keepPreviousData`, the page stays put and `isFetching` narrates progress.
    placeholderData: keepPreviousData,
    refetchInterval: 60_000,
  });
}
