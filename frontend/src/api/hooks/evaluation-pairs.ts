/**
 * Paginated pair-detail feed for the Candidates drilldown dialog.
 *
 * Wraps `GET /api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details`
 * in a `useInfiniteQuery` — pages accumulate in one cache entry keyed on
 * `(campaignId, destinationIp, query)` and `fetchNextPage()` drives the
 * dialog's virtualized scroll-append flow. Hostname seeding via
 * `useSeedHostnamesOnResponse` keeps the shared IP→hostname map warm
 * without forcing every drilldown row to re-fetch on its own.
 *
 * Cache invalidation: this key sits under `campaignEvaluationKey(id)`
 * by construction (`campaignEvaluationCandidatePairsKey` prepends it),
 * so the campaign-stream `evaluated`-event handler — which already
 * invalidates the evaluation key — cascades to every active drilldown
 * query. See `campaign-stream.ts` for the load-bearing comment.
 */

import {
  type InfiniteData,
  type UseInfiniteQueryResult,
  useInfiniteQuery,
} from "@tanstack/react-query";
import { api } from "@/api/client";
import { campaignEvaluationKey } from "@/api/hooks/campaigns";
import type { components } from "@/api/schema.gen";
import { useSeedHostnamesOnResponse } from "@/components/ip-hostname";

export type EvaluationPairDetailListResponse =
  components["schemas"]["EvaluationPairDetailListResponse"];
export type EvaluationPairDetail = components["schemas"]["EvaluationPairDetailDto"];
export type PairDetailSortCol = components["schemas"]["PairDetailSortCol"];
export type PairDetailSortDir = components["schemas"]["PairDetailSortDir"];

/**
 * Toolbar / sort state passed into [`useCandidatePairDetails`].
 *
 * Numeric runtime filters are nullable — `null` (or `undefined`) means
 * "knob unset; gate is open", consistent with the backend's `Option<f64>`
 * semantics. The serializer below collapses `null` and `undefined` into
 * a missing query param so the wire is unambiguous.
 */
export interface PairDetailsQuery {
  sort: PairDetailSortCol;
  dir: PairDetailSortDir;
  min_improvement_ms?: number | null;
  min_improvement_ratio?: number | null;
  max_transit_rtt_ms?: number | null;
  max_transit_stddev_ms?: number | null;
  qualifies_only?: boolean | null;
  limit?: number;
}

/**
 * TanStack Query cache key for a paginated pair-detail feed.
 *
 * The key prepends `campaignEvaluationKey(campaignId)` so the campaign
 * SSE listener's `evaluated`-event invalidation cascades to every
 * cached drilldown variant without a dedicated branch.
 */
export function campaignEvaluationCandidatePairsKey(
  campaignId: string,
  destinationIp: string,
  query: PairDetailsQuery,
) {
  return [
    ...campaignEvaluationKey(campaignId),
    "candidates",
    destinationIp,
    "pair_details",
    query,
  ] as const;
}

export function useCandidatePairDetails(
  campaignId: string | undefined,
  destinationIp: string | undefined,
  query: PairDetailsQuery,
): UseInfiniteQueryResult<InfiniteData<EvaluationPairDetailListResponse>, Error> {
  const enabled = !!campaignId && !!destinationIp;
  const result = useInfiniteQuery<
    EvaluationPairDetailListResponse,
    Error,
    InfiniteData<EvaluationPairDetailListResponse>,
    readonly unknown[],
    string | null
  >({
    queryKey: enabled
      ? campaignEvaluationCandidatePairsKey(campaignId, destinationIp, query)
      : ["campaigns", "entry", "__disabled__", "evaluation", "pair_details"],
    enabled,
    initialPageParam: null as string | null,
    queryFn: async ({ pageParam }): Promise<EvaluationPairDetailListResponse> => {
      // queryFn only runs when enabled → ids are defined.
      const cid = campaignId as string;
      const dip = destinationIp as string;
      const { data, error } = await api.GET(
        "/api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details",
        {
          params: {
            path: { id: cid, destination_ip: dip },
            query: {
              sort: query.sort,
              dir: query.dir,
              ...(query.min_improvement_ms != null
                ? { min_improvement_ms: query.min_improvement_ms }
                : {}),
              ...(query.min_improvement_ratio != null
                ? { min_improvement_ratio: query.min_improvement_ratio }
                : {}),
              ...(query.max_transit_rtt_ms != null
                ? { max_transit_rtt_ms: query.max_transit_rtt_ms }
                : {}),
              ...(query.max_transit_stddev_ms != null
                ? { max_transit_stddev_ms: query.max_transit_stddev_ms }
                : {}),
              ...(query.qualifies_only != null ? { qualifies_only: query.qualifies_only } : {}),
              ...(query.limit !== undefined ? { limit: query.limit } : {}),
              ...(pageParam ? { cursor: pageParam } : {}),
            },
          },
        },
      );
      if (error) throw new Error("failed to fetch pair details", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
  });

  // Seed the shared hostname map from every loaded page. Each entry's
  // `destination_ip` mirrors the candidate transit IP, so the seed is
  // largely redundant with the candidate-level stamp on `useEvaluation`,
  // but keeping it here keeps the dialog self-contained on cold starts
  // (e.g. a deep link straight into a candidate that the candidates
  // table never rendered).
  useSeedHostnamesOnResponse(result.data?.pages, function* (pages) {
    for (const page of pages) {
      for (const entry of page.entries) {
        yield { ip: entry.destination_ip, hostname: entry.destination_hostname };
      }
    }
  });

  return result;
}
