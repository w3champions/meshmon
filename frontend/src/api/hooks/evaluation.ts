/**
 * Evaluation and detail-trigger mutations for the Results browser.
 *
 * `useEvaluation` treats a 404 as "not yet evaluated" and resolves to `null`
 * so consumers can branch on `data === null` rather than inspecting the query
 * error. `useEvaluateCampaign` seeds the cache directly from its mutation
 * response — the SSE `evaluated` event fires via Postgres NOTIFY after COMMIT,
 * so a client that subscribes after the commit misses its own frame.
 * `useTriggerDetail` invalidates the `measurements` prefix so the Raw tab
 * refetches after a dispatch creates new pairs.
 * `useEdgePairDetails` is the paginated feed for the edge-candidate evaluation
 * edge-pairs table (mode=`edge_candidate` only).
 */

import {
  type InfiniteData,
  type UseInfiniteQueryResult,
  type UseMutationResult,
  type UseQueryResult,
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { api } from "@/api/client";
import {
  campaignEdgePairsKey,
  campaignEvaluationKey,
  campaignKey,
  campaignMeasurementsPrefixKey,
  campaignPairsKey,
  campaignPreviewKey,
  type EdgePairsQuery,
} from "@/api/hooks/campaigns";
import type { components } from "@/api/schema.gen";
import { useSeedHostnamesOnResponse } from "@/components/ip-hostname";

export type Evaluation = components["schemas"]["EvaluationDto"];
export type DetailRequest = components["schemas"]["DetailRequest"];
export type DetailResponse = components["schemas"]["DetailResponse"];
export type DetailScope = components["schemas"]["DetailScope"];
export type EdgePairsListResponse = components["schemas"]["EdgePairsListResponse"];
export type EvaluationEdgePairDetailDto = components["schemas"]["EvaluationEdgePairDetailDto"];

/**
 * Fetch a campaign's evaluation row. Returns `null` on 404 so the caller can
 * distinguish "never evaluated" from "still loading" without inspecting the
 * query error.
 */
export function useEvaluation(id: string | undefined): UseQueryResult<Evaluation | null, Error> {
  const query = useQuery({
    queryKey: id ? campaignEvaluationKey(id) : ["campaigns", "entry", "__disabled__", "evaluation"],
    enabled: !!id,
    queryFn: async (): Promise<Evaluation | null> => {
      // queryFn only runs when enabled → id is defined.
      const campaignId = id as string;
      const { data, error, response } = await api.GET("/api/campaigns/{id}/evaluation", {
        params: { path: { id: campaignId } },
      });
      if (response?.status === 404) return null;
      if (error) throw new Error("failed to fetch evaluation", { cause: error });
      if (!data) throw new Error("empty response");
      return data as Evaluation;
    },
  });
  // Seed the shared hostname map from every response. Pair-detail rows
  // are not carried on the candidate's wire shape — they live behind
  // the paginated `…/candidates/{ip}/pair_details` endpoint, where
  // [`useCandidatePairDetails`] runs its own `useSeedHostnamesOnResponse`
  // pass. Here we only yield the candidate IP + hostname pair. Null
  // (404 / not-yet-evaluated) produces no entries.
  useSeedHostnamesOnResponse(query.data, function* (evaluation) {
    if (!evaluation) return;
    for (const candidate of evaluation.results.candidates) {
      yield { ip: candidate.destination_ip, hostname: candidate.hostname };
    }
  });
  return query;
}

/**
 * Kick off the evaluator for a terminal campaign. Seeds the evaluation cache
 * from the returned row so the UI does not have to wait for the SSE
 * `evaluated` frame (which may not arrive if this client subscribed after the
 * COMMIT). Also invalidates the campaign shell key (state + `evaluated_at`),
 * the pairs list and preview (evaluator may have regenerated baseline pairs),
 * and the measurements prefix so the Raw tab refetches — parallel to the
 * invalidation set `useTriggerDetail` applies after detail dispatch.
 */
export function useEvaluateCampaign(): UseMutationResult<Evaluation, Error, string> {
  const queryClient = useQueryClient();
  return useMutation<Evaluation, Error, string>({
    mutationFn: async (id): Promise<Evaluation> => {
      const { data, error } = await api.POST("/api/campaigns/{id}/evaluate", {
        params: { path: { id } },
      });
      if (error) throw new Error("failed to evaluate", { cause: error });
      if (!data) throw new Error("empty response");
      return data as Evaluation;
    },
    onSuccess: (data, id) => {
      // The returned row is identical to what the next GET /evaluation would
      // produce, so seed the cache directly and skip a refetch round-trip.
      queryClient.setQueryData(campaignEvaluationKey(id), data);
      queryClient.invalidateQueries({ queryKey: campaignKey(id) });
      queryClient.invalidateQueries({ queryKey: campaignPairsKey(id) });
      queryClient.invalidateQueries({ queryKey: campaignPreviewKey(id) });
      // Prefix invalidation so every filter variant of the Raw tab refetches.
      queryClient.invalidateQueries({ queryKey: campaignMeasurementsPrefixKey(id) });
    },
  });
}

export interface TriggerDetailVariables {
  id: string;
  body: DetailRequest;
}

/**
 * Dispatch detail-grade re-measurement for a campaign slice. The scheduler
 * flips state back to `running` and creates new `campaign_pairs` rows; the
 * pair list, dispatch preview, and campaign-scoped measurements feed all
 * shift, so we invalidate each. The evaluation row is intentionally left
 * alone — detail data refines the baseline, not replaces it.
 */
export function useTriggerDetail(): UseMutationResult<
  DetailResponse,
  Error,
  TriggerDetailVariables
> {
  const queryClient = useQueryClient();
  return useMutation<DetailResponse, Error, TriggerDetailVariables>({
    mutationFn: async ({ id, body }): Promise<DetailResponse> => {
      const { data, error } = await api.POST("/api/campaigns/{id}/detail", {
        params: { path: { id } },
        body,
      });
      if (error) throw new Error("failed to trigger detail", { cause: error });
      if (!data) throw new Error("empty response");
      return data as DetailResponse;
    },
    onSuccess: (_data, { id }) => {
      queryClient.invalidateQueries({ queryKey: campaignKey(id) });
      queryClient.invalidateQueries({ queryKey: campaignPairsKey(id) });
      queryClient.invalidateQueries({ queryKey: campaignPreviewKey(id) });
      // Prefix invalidation so every filter variant of the Raw tab refetches.
      queryClient.invalidateQueries({ queryKey: campaignMeasurementsPrefixKey(id) });
    },
  });
}

/**
 * Infinite-cursor feed for `GET /api/campaigns/{id}/evaluation/edge_pairs`.
 *
 * Only applicable to `edge_candidate`-mode campaigns. Pages accumulate in a
 * single cache entry keyed on `(campaignId, query)`; the table calls
 * `fetchNextPage()` as the virtualized list approaches the bottom. Disabled
 * while `campaignId` is undefined.
 *
 * Cache invalidation: the SSE `evaluated` handler in `campaign-stream.ts`
 * calls `invalidateQueries({ queryKey: campaignEdgePairsPrefixKey(id) })`,
 * which cascades to every active filter variant by prefix match.
 */
export function useEdgePairDetails(
  campaignId: string | undefined,
  query: EdgePairsQuery,
): UseInfiniteQueryResult<InfiniteData<EdgePairsListResponse>, Error> {
  return useInfiniteQuery<
    EdgePairsListResponse,
    Error,
    InfiniteData<EdgePairsListResponse>,
    readonly unknown[],
    string | null
  >({
    queryKey: campaignId
      ? campaignEdgePairsKey(campaignId, query)
      : ["campaigns", "entry", "__disabled__", "edge_pairs"],
    enabled: !!campaignId,
    initialPageParam: null as string | null,
    queryFn: async ({ pageParam }): Promise<EdgePairsListResponse> => {
      // queryFn only runs when enabled → campaignId is defined.
      const id = campaignId as string;
      const { data, error } = await api.GET("/api/campaigns/{id}/evaluation/edge_pairs", {
        params: {
          path: { id },
          query: {
            ...(query.candidate_ip ? { candidate_ip: query.candidate_ip } : {}),
            ...(query.qualifies_only != null ? { qualifies_only: query.qualifies_only } : {}),
            ...(query.reachable_only != null ? { reachable_only: query.reachable_only } : {}),
            ...(query.sort ? { sort: query.sort } : {}),
            ...(query.dir ? { dir: query.dir } : {}),
            ...(query.limit !== undefined ? { limit: query.limit } : {}),
            ...(pageParam ? { cursor: pageParam } : {}),
          },
        },
      });
      if (error) throw new Error("failed to fetch edge pairs", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? null,
  });
}
