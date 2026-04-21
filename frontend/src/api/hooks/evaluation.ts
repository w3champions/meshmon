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
 */

import {
  type UseMutationResult,
  type UseQueryResult,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { api } from "@/api/client";
import {
  campaignEvaluationKey,
  campaignKey,
  campaignMeasurementsPrefixKey,
  campaignPairsKey,
  campaignPreviewKey,
} from "@/api/hooks/campaigns";
import type { components } from "@/api/schema.gen";

export type Evaluation = components["schemas"]["EvaluationDto"];
export type DetailRequest = components["schemas"]["DetailRequest"];
export type DetailResponse = components["schemas"]["DetailResponse"];
export type DetailScope = components["schemas"]["DetailScope"];

/**
 * Fetch a campaign's evaluation row. Returns `null` on 404 so the caller can
 * distinguish "never evaluated" from "still loading" without inspecting the
 * query error.
 */
export function useEvaluation(id: string | undefined): UseQueryResult<Evaluation | null, Error> {
  return useQuery({
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
}

/**
 * Kick off the evaluator for a terminal campaign. Seeds the evaluation cache
 * from the returned row so the UI does not have to wait for the SSE
 * `evaluated` frame (which may not arrive if this client subscribed after the
 * COMMIT). Also invalidates the campaign shell key since `state` and
 * `evaluated_at` have moved.
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
