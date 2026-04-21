import {
  type UseMutationResult,
  type UseQueryResult,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { api } from "@/api/client";
import type { components, operations } from "@/api/schema.gen";

export type Campaign = components["schemas"]["CampaignDto"];
export type CreateCampaignBody = components["schemas"]["CreateCampaignRequest"];
export type PatchCampaignBody = components["schemas"]["PatchCampaignRequest"];
export type EditCampaignBody = components["schemas"]["EditCampaignRequest"];
export type ForcePairBody = components["schemas"]["ForcePairRequest"];
export type PreviewDispatchResponse = components["schemas"]["PreviewDispatchResponse"];
export type CampaignState = components["schemas"]["CampaignState"];
export type EvaluationMode = components["schemas"]["EvaluationMode"];
export type ProbeProtocol = components["schemas"]["ProbeProtocol"];
export type PairResolutionState = components["schemas"]["PairResolutionState"];

/**
 * Query shape for `GET /api/campaigns`. Sourced directly from the generated
 * OpenAPI spec so there is a single source of truth for supported filters.
 */
export type CampaignListQuery = NonNullable<operations["campaigns_list"]["parameters"]["query"]>;

export const CAMPAIGNS_LIST_KEY = ["campaigns", "list"] as const;
export const CAMPAIGN_PREVIEW_KEY = ["campaigns", "preview"] as const;

export function campaignKey(id: string) {
  return ["campaigns", "entry", id] as const;
}

export function campaignPairsKey(id: string) {
  return ["campaigns", "entry", id, "pairs"] as const;
}

export function campaignPreviewKey(id: string) {
  return ["campaigns", "preview", id] as const;
}

/**
 * Query key for a campaign's evaluation read. The GET `/evaluation` read hook
 * is wired in a later task (T49); the key is exported here so the SSE stream
 * can invalidate it when the broker emits an `evaluated` frame — the invalidate
 * is a no-op until a consumer subscribes.
 */
export function campaignEvaluationKey(id: string) {
  return ["campaigns", "entry", id, "evaluation"] as const;
}

/** Polling cadence for the filtered campaign list. */
const CAMPAIGNS_LIST_REFETCH_MS = 15_000;

/** Polling cadence for the dispatch preview while a campaign is active. */
const CAMPAIGN_PREVIEW_REFETCH_MS = 5_000;

/**
 * Fetch a filtered page of campaigns. The SSE stream (`useCampaignStream`)
 * also invalidates this key on lifecycle changes, so the 15s polling is a
 * safety net rather than the primary freshness mechanism.
 */
export function useCampaignsList(query: CampaignListQuery): UseQueryResult<Campaign[], Error> {
  return useQuery({
    queryKey: [...CAMPAIGNS_LIST_KEY, query],
    refetchInterval: CAMPAIGNS_LIST_REFETCH_MS,
    queryFn: async (): Promise<Campaign[]> => {
      const { data, error } = await api.GET("/api/campaigns", {
        params: { query },
      });
      if (error) throw new Error("failed to fetch campaigns", { cause: error });
      if (!data) throw new Error("empty response");
      // Cast through the component schema: openapi-typescript widens the
      // inline response tuple (`pair_counts: [state, number][]`) to
      // `(state | number)[][]` when it re-inlines the response body, so the
      // direct return is not assignable to the component alias without this
      // hop. The runtime shape is identical.
      return data as Campaign[];
    },
  });
}

/**
 * Single-row fetch for a campaign. Returns `null` for 404 so the caller can
 * distinguish "not found" from "still loading" without inspecting the query
 * error.
 */
export function useCampaign(id: string | undefined): UseQueryResult<Campaign | null, Error> {
  return useQuery({
    queryKey: id ? campaignKey(id) : ["campaigns", "entry", "__disabled__"],
    enabled: !!id,
    queryFn: async (): Promise<Campaign | null> => {
      // queryFn only runs when enabled → id is defined.
      const campaignId = id as string;
      const { data, error, response } = await api.GET("/api/campaigns/{id}", {
        params: { path: { id: campaignId } },
      });
      if (response?.status === 404) return null;
      if (error) throw new Error("failed to fetch campaign", { cause: error });
      if (!data) throw new Error("empty response");
      return data as Campaign;
    },
  });
}

/**
 * Fetch the dispatch preview (`fresh` / `reusable` / `total`) for a campaign.
 * Polls on a 5s cadence while the hook is mounted — a running campaign's
 * preview shifts as pairs settle.
 */
export function usePreviewDispatchCount(
  id: string | undefined,
): UseQueryResult<PreviewDispatchResponse, Error> {
  return useQuery({
    queryKey: id ? campaignPreviewKey(id) : ["campaigns", "preview", "__disabled__"],
    enabled: !!id,
    refetchInterval: CAMPAIGN_PREVIEW_REFETCH_MS,
    queryFn: async (): Promise<PreviewDispatchResponse> => {
      // queryFn only runs when enabled → id is defined.
      const campaignId = id as string;
      const { data, error } = await api.GET("/api/campaigns/{id}/preview-dispatch-count", {
        params: { path: { id: campaignId } },
      });
      if (error) throw new Error("failed to fetch dispatch preview", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
  });
}

/**
 * Create a campaign in `draft`. Invalidates the list key so the newly-created
 * row surfaces immediately; the single-row cache is seeded by the returned
 * mutation data.
 */
export function useCreateCampaign(): UseMutationResult<Campaign, Error, CreateCampaignBody> {
  const queryClient = useQueryClient();
  return useMutation<Campaign, Error, CreateCampaignBody>({
    mutationFn: async (body): Promise<Campaign> => {
      const { data, error } = await api.POST("/api/campaigns", { body });
      if (error) throw new Error("failed to create campaign", { cause: error });
      if (!data) throw new Error("empty response");
      return data as Campaign;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: CAMPAIGNS_LIST_KEY });
    },
  });
}

export interface PatchCampaignVariables {
  id: string;
  body: PatchCampaignBody;
}

/**
 * Partial update of a draft campaign. Lifecycle state is owned by the
 * start/stop/edit endpoints, so this hook intentionally does not invalidate
 * `campaignPreviewKey` — the preview only moves when pairs change.
 */
export function usePatchCampaign(): UseMutationResult<Campaign, Error, PatchCampaignVariables> {
  const queryClient = useQueryClient();
  return useMutation<Campaign, Error, PatchCampaignVariables>({
    mutationFn: async ({ id, body }): Promise<Campaign> => {
      const { data, error } = await api.PATCH("/api/campaigns/{id}", {
        params: { path: { id } },
        body,
      });
      if (error) throw new Error("failed to patch campaign", { cause: error });
      if (!data) throw new Error("empty response");
      return data as Campaign;
    },
    onSuccess: (data, { id }) => {
      queryClient.setQueryData(campaignKey(id), data);
      queryClient.invalidateQueries({ queryKey: CAMPAIGNS_LIST_KEY });
    },
  });
}

/**
 * Transition `draft` → `running`. Also invalidates the preview key because
 * `started_at` moves and the scheduler begins consuming pairs.
 */
export function useStartCampaign(): UseMutationResult<Campaign, Error, string> {
  const queryClient = useQueryClient();
  return useMutation<Campaign, Error, string>({
    mutationFn: async (id): Promise<Campaign> => {
      const { data, error } = await api.POST("/api/campaigns/{id}/start", {
        params: { path: { id } },
      });
      if (error) throw new Error("failed to start campaign", { cause: error });
      if (!data) throw new Error("empty response");
      return data as Campaign;
    },
    onSuccess: (data, id) => {
      queryClient.setQueryData(campaignKey(id), data);
      queryClient.invalidateQueries({ queryKey: CAMPAIGNS_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: campaignPreviewKey(id) });
    },
  });
}

/**
 * Transition `running` → `stopped`. Pending pairs flip to `skipped` server-side,
 * which shifts `fresh`/`reusable` in the preview, so we invalidate it alongside
 * the single-row and list keys.
 */
export function useStopCampaign(): UseMutationResult<Campaign, Error, string> {
  const queryClient = useQueryClient();
  return useMutation<Campaign, Error, string>({
    mutationFn: async (id): Promise<Campaign> => {
      const { data, error } = await api.POST("/api/campaigns/{id}/stop", {
        params: { path: { id } },
      });
      if (error) throw new Error("failed to stop campaign", { cause: error });
      if (!data) throw new Error("empty response");
      return data as Campaign;
    },
    onSuccess: (data, id) => {
      queryClient.setQueryData(campaignKey(id), data);
      queryClient.invalidateQueries({ queryKey: CAMPAIGNS_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: campaignPreviewKey(id) });
    },
  });
}

export interface EditCampaignVariables {
  id: string;
  body: EditCampaignBody;
}

/**
 * Apply an edit delta to a finished campaign. The server re-enters `running`
 * on success, which can both add/remove pairs and (with `force_measurement`)
 * reset the whole campaign — invalidate the preview key so the dispatch
 * estimate refreshes immediately.
 */
export function useEditCampaign(): UseMutationResult<Campaign, Error, EditCampaignVariables> {
  const queryClient = useQueryClient();
  return useMutation<Campaign, Error, EditCampaignVariables>({
    mutationFn: async ({ id, body }): Promise<Campaign> => {
      const { data, error } = await api.POST("/api/campaigns/{id}/edit", {
        params: { path: { id } },
        body,
      });
      if (error) throw new Error("failed to edit campaign", { cause: error });
      if (!data) throw new Error("empty response");
      return data as Campaign;
    },
    onSuccess: (data, { id }) => {
      queryClient.setQueryData(campaignKey(id), data);
      queryClient.invalidateQueries({ queryKey: CAMPAIGNS_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: campaignPreviewKey(id) });
    },
  });
}

/**
 * Idempotent delete. The server returns 204 whether or not the row existed;
 * we remove the per-row cache unconditionally and invalidate the list.
 */
export function useDeleteCampaign(): UseMutationResult<void, Error, string> {
  const queryClient = useQueryClient();
  return useMutation<void, Error, string>({
    mutationFn: async (id): Promise<void> => {
      const { error } = await api.DELETE("/api/campaigns/{id}", {
        params: { path: { id } },
      });
      if (error) throw new Error("failed to delete campaign", { cause: error });
    },
    onSuccess: (_data, id) => {
      queryClient.removeQueries({ queryKey: campaignKey(id) });
      queryClient.invalidateQueries({ queryKey: CAMPAIGNS_LIST_KEY });
    },
  });
}

export interface ForcePairVariables {
  id: string;
  body: ForcePairBody;
}

/**
 * Force-reset a single pair and re-enter `running`. The campaign shell, the
 * paginated pair list, and the dispatch preview all shift — invalidate
 * all three.
 */
export function useForcePair(): UseMutationResult<Campaign, Error, ForcePairVariables> {
  const queryClient = useQueryClient();
  return useMutation<Campaign, Error, ForcePairVariables>({
    mutationFn: async ({ id, body }): Promise<Campaign> => {
      const { data, error } = await api.POST("/api/campaigns/{id}/force_pair", {
        params: { path: { id } },
        body,
      });
      if (error) throw new Error("failed to force pair", { cause: error });
      if (!data) throw new Error("empty response");
      return data as Campaign;
    },
    onSuccess: (data, { id }) => {
      queryClient.setQueryData(campaignKey(id), data);
      queryClient.invalidateQueries({ queryKey: campaignPairsKey(id) });
      queryClient.invalidateQueries({ queryKey: campaignPreviewKey(id) });
    },
  });
}
