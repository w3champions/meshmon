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
export type MeasurementKind = components["schemas"]["MeasurementKind"];
export type CampaignMeasurement = components["schemas"]["CampaignMeasurementDto"];
export type CampaignMeasurementsPage = components["schemas"]["CampaignMeasurementsPage"];
export type CampaignPair = components["schemas"]["PairDto"];

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

/**
 * Prefix key covering every cached `/measurements` page for a campaign
 * (regardless of filter). Used by the SSE `pair_settled` handler and by
 * `useTriggerDetail`'s `onSuccess` so in-flight detail rows surface live on
 * the Raw tab.
 */
export function campaignMeasurementsPrefixKey(id: string) {
  return ["campaigns", "entry", id, "measurements"] as const;
}

/**
 * Narrow variant of {@link campaignMeasurementsPrefixKey} that also keys on
 * the active filter set. Each filter permutation is a separate cache entry so
 * the Raw tab can switch facets without stomping a sibling view; prefix
 * invalidation from SSE / detail-trigger sweeps them all at once.
 */
export function campaignMeasurementsKey(id: string, filter: CampaignMeasurementsFilter) {
  return ["campaigns", "entry", id, "measurements", filter] as const;
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
 * estimate refreshes immediately, and the measurements prefix so the Raw tab
 * drops any rows that belonged to now-removed pairs or a force-reset sweep.
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
      // Prefix invalidation so every filter variant of the Raw tab refetches —
      // an edit that removes pairs or sets `force_measurement` can mutate the
      // measurement set without emitting a matching SSE `pair_settled`.
      queryClient.invalidateQueries({ queryKey: campaignMeasurementsPrefixKey(id) });
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
 * Filters for the campaign-scoped measurements feed (Raw tab).
 *
 * Cursor is intentionally absent — it's threaded through `useInfiniteQuery`'s
 * `pageParam` so every page of a single filter permutation collapses onto
 * one cache entry, and `fetchNextPage()` drives the scroll-append flow in
 * the Raw tab's virtualized list. `measurement_id` supports the
 * DrilldownDrawer's single-row MTR resolution.
 */
export interface CampaignMeasurementsFilter {
  resolution_state?: PairResolutionState;
  protocol?: ProbeProtocol;
  kind?: MeasurementKind;
  measurement_id?: number;
  limit?: number;
}

/**
 * Infinite-cursor fetch over the campaign's joined
 * `campaign_pairs → measurements` feed. The endpoint paginates via a keyset
 * cursor in `measured_at DESC NULLS LAST, cp.id DESC` order; pages are
 * accumulated in a single cache entry keyed on `(id, filter)` (no cursor in
 * the key), and the Raw tab calls `fetchNextPage()` as the virtualized list
 * approaches the bottom. Disabled while `id` is undefined.
 */
export function useCampaignMeasurements(
  id: string | undefined,
  filter: CampaignMeasurementsFilter,
): UseInfiniteQueryResult<InfiniteData<CampaignMeasurementsPage>, Error> {
  return useInfiniteQuery<
    CampaignMeasurementsPage,
    Error,
    InfiniteData<CampaignMeasurementsPage>,
    readonly unknown[],
    string | null
  >({
    queryKey: id
      ? campaignMeasurementsKey(id, filter)
      : ["campaigns", "entry", "__disabled__", "measurements"],
    enabled: !!id,
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? null,
    queryFn: async ({ pageParam }): Promise<CampaignMeasurementsPage> => {
      // queryFn only runs when enabled → id is defined.
      const campaignId = id as string;
      const { data, error } = await api.GET("/api/campaigns/{id}/measurements", {
        params: {
          path: { id: campaignId },
          query: {
            ...(filter.resolution_state ? { resolution_state: filter.resolution_state } : {}),
            ...(filter.protocol ? { protocol: filter.protocol } : {}),
            ...(filter.kind ? { kind: filter.kind } : {}),
            ...(filter.measurement_id !== undefined
              ? { measurement_id: filter.measurement_id }
              : {}),
            ...(pageParam ? { cursor: pageParam } : {}),
            ...(filter.limit !== undefined ? { limit: filter.limit } : {}),
          },
        },
      });
      if (error) throw new Error("failed to fetch campaign measurements", { cause: error });
      if (!data) throw new Error("empty response");
      return data as CampaignMeasurementsPage;
    },
  });
}

/**
 * Filter shape for `GET /api/campaigns/{id}/pairs`. Empty `state` expands
 * server-side to "every resolution state"; the endpoint is limit-paginated
 * only (no cursor), so callers render the full response in one shot.
 */
export interface CampaignPairsFilter {
  state?: PairResolutionState[];
  limit?: number;
}

/**
 * Fetch the campaign's full pair list. The endpoint is limit-paginated —
 * no cursor — so callers materialise the entire (bounded) response in one
 * query. The scheduler bumps `pair_counts` on every `campaign_pair_settled`
 * NOTIFY, which invalidates the campaign shell key; the SSE stream does
 * NOT touch `campaignPairsKey` today, so `refetchInterval: 15s` keeps the
 * Pairs tab eventually-consistent without a dedicated stream subscription.
 * A force-pair or detail-trigger mutation invalidates this key eagerly.
 */
const CAMPAIGN_PAIRS_REFETCH_MS = 15_000;

export function useCampaignPairs(
  id: string | undefined,
  filter: CampaignPairsFilter = {},
): UseQueryResult<CampaignPair[], Error> {
  return useQuery({
    queryKey: id ? [...campaignPairsKey(id), filter] : ["campaigns", "entry", "__disabled__", "pairs"],
    enabled: !!id,
    refetchInterval: CAMPAIGN_PAIRS_REFETCH_MS,
    queryFn: async (): Promise<CampaignPair[]> => {
      // queryFn only runs when enabled → id is defined.
      const campaignId = id as string;
      const { data, error } = await api.GET("/api/campaigns/{id}/pairs", {
        params: {
          path: { id: campaignId },
          query: {
            ...(filter.state && filter.state.length > 0 ? { state: filter.state } : {}),
            ...(filter.limit !== undefined ? { limit: filter.limit } : {}),
          },
        },
      });
      if (error) throw new Error("failed to fetch campaign pairs", { cause: error });
      if (!data) throw new Error("empty response");
      return data as CampaignPair[];
    },
  });
}

/**
 * Force-reset a single pair and re-enter `running`. The campaign shell, the
 * paginated pair list, and the dispatch preview all shift — invalidate
 * all three. Also invalidate the measurements prefix: when the force-pair
 * outcome reuses an existing measurement the writer does not emit a
 * `pair_settled` frame, so the Raw tab would otherwise stay stale.
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
      queryClient.invalidateQueries({ queryKey: campaignMeasurementsPrefixKey(id) });
    },
  });
}
