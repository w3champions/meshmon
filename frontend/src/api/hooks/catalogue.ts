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

export type CatalogueEntry = components["schemas"]["CatalogueEntryDto"];
export type CatalogueListResponse = components["schemas"]["ListResponse"];
export type CatalogueFacets = components["schemas"]["FacetsResponse"];
export type CataloguePasteRequest = components["schemas"]["PasteRequest"];
export type CataloguePasteResponse = components["schemas"]["PasteResponse"];
export type CataloguePatchRequest = components["schemas"]["PatchRequest"];
export type CatalogueBulkReenrichRequest = components["schemas"]["BulkReenrichRequest"];
export type CatalogueMapResponse = components["schemas"]["MapResponse"];
export type CatalogueMapBucket = components["schemas"]["MapBucket"];

/**
 * Query shape for `GET /api/catalogue`, minus the `after` cursor and `limit`
 * (both are owned by `useCatalogueListInfinite`). Sourced directly from the
 * generated OpenAPI spec so there is a single source of truth for supported
 * filters.
 */
export type CatalogueListQuery = Omit<
  NonNullable<operations["list"]["parameters"]["query"]>,
  "after" | "limit"
>;

/**
 * Query shape for `GET /api/catalogue/map`, minus the viewport (`bbox`) and
 * `zoom` — those are always supplied at call sites because the map is
 * viewport-driven. `shapes`, `sort`, `sort_dir`, `after`, `city` are not part
 * of the map surface server-side, so they do not appear here either.
 */
export type CatalogueMapQuery = Omit<
  NonNullable<operations["map"]["parameters"]["query"]>,
  "bbox" | "zoom"
>;

export const CATALOGUE_LIST_KEY = ["catalogue", "list"] as const;
export const CATALOGUE_MAP_KEY = ["catalogue", "map"] as const;
export const CATALOGUE_FACETS_KEY = ["catalogue", "facets"] as const;

export function catalogueEntryKey(id: string) {
  return ["catalogue", "entry", id] as const;
}

export interface CatalogueListInfiniteOptions {
  /** Page size; clamped server-side to `1..=500`. Default 100. */
  pageSize?: number;
}

/** Default `limit` used when no `pageSize` option is supplied. */
const DEFAULT_PAGE_SIZE = 100;

/**
 * Cursor through `GET /api/catalogue` using keyset paging.
 *
 * Each page's `next_cursor` is fed back in as the `after` query param of the
 * following page; a `null` cursor terminates the sequence. The query key
 * embeds the full filter object plus `pageSize`, so a filter change spawns a
 * fresh cursor chain rather than polluting the previous one's pages.
 */
export function useCatalogueListInfinite(
  query: CatalogueListQuery = {},
  options: CatalogueListInfiniteOptions = {},
): UseInfiniteQueryResult<InfiniteData<CatalogueListResponse>, Error> {
  const pageSize = options.pageSize ?? DEFAULT_PAGE_SIZE;
  return useInfiniteQuery<
    CatalogueListResponse,
    Error,
    InfiniteData<CatalogueListResponse>,
    readonly unknown[],
    string | undefined
  >({
    queryKey: [...CATALOGUE_LIST_KEY, query, pageSize],
    initialPageParam: undefined,
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
    queryFn: async ({ pageParam }): Promise<CatalogueListResponse> => {
      const { data, error } = await api.GET("/api/catalogue", {
        params: {
          query: {
            ...query,
            limit: pageSize,
            after: pageParam,
          },
        },
      });
      if (error) throw new Error("failed to fetch catalogue", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
  });
}

/**
 * Fetch the adaptive map response for a given viewport.
 *
 * Disabled while `bbox` is absent so a just-mounted map doesn't emit a bad
 * `bbox=` query (the backend treats missing/malformed `bbox` as a 400).
 * Once `bbox` is defined, the query refires whenever the viewport, zoom, or
 * any filter changes — query-key equality handles it automatically.
 *
 * The backend picks detail-vs-cluster internally; consumers branch on
 * `data.kind` which narrows the union to the right shape.
 */
export function useCatalogueMap(
  bbox: readonly [number, number, number, number] | undefined,
  zoom: number,
  filters: CatalogueMapQuery = {},
): UseQueryResult<CatalogueMapResponse, Error> {
  return useQuery({
    queryKey: [...CATALOGUE_MAP_KEY, bbox, zoom, filters],
    enabled: bbox !== undefined,
    queryFn: async (): Promise<CatalogueMapResponse> => {
      // queryFn only runs when enabled → bbox is defined.
      const bboxValue = bbox as readonly [number, number, number, number];
      const { data, error } = await api.GET("/api/catalogue/map", {
        params: {
          query: {
            ...filters,
            bbox: [...bboxValue],
            zoom,
          },
        },
      });
      if (error) throw new Error("failed to fetch catalogue map", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
  });
}

export function useCatalogueEntry(
  id: string | undefined,
): UseQueryResult<CatalogueEntry | null, Error> {
  return useQuery({
    queryKey: id ? catalogueEntryKey(id) : ["catalogue", "entry", "__disabled__"],
    enabled: !!id,
    queryFn: async (): Promise<CatalogueEntry | null> => {
      // queryFn only runs when enabled → id is defined.
      const entryId = id as string;
      const { data, error, response } = await api.GET("/api/catalogue/{id}", {
        params: { path: { id: entryId } },
      });
      if (response?.status === 404) return null;
      if (error) throw new Error("failed to fetch catalogue entry", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
  });
}

export function useCatalogueFacets(): UseQueryResult<CatalogueFacets, Error> {
  return useQuery({
    queryKey: CATALOGUE_FACETS_KEY,
    staleTime: 30_000,
    queryFn: async (): Promise<CatalogueFacets> => {
      const { data, error } = await api.GET("/api/catalogue/facets");
      if (error) throw new Error("failed to fetch catalogue facets", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
  });
}

export function usePasteCatalogue(): UseMutationResult<
  CataloguePasteResponse,
  Error,
  CataloguePasteRequest
> {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (body): Promise<CataloguePasteResponse> => {
      const { data, error } = await api.POST("/api/catalogue", { body });
      if (error) throw new Error("failed to paste catalogue", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: CATALOGUE_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_MAP_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
    },
  });
}

export interface PatchCatalogueVariables {
  id: string;
  patch: CataloguePatchRequest;
}

interface PatchContext {
  previous: CatalogueEntry | undefined;
}

export function usePatchCatalogueEntry(): UseMutationResult<
  CatalogueEntry,
  Error,
  PatchCatalogueVariables,
  PatchContext
> {
  const queryClient = useQueryClient();
  return useMutation<CatalogueEntry, Error, PatchCatalogueVariables, PatchContext>({
    mutationFn: async ({ id, patch }): Promise<CatalogueEntry> => {
      const { data, error } = await api.PATCH("/api/catalogue/{id}", {
        params: { path: { id } },
        body: patch,
      });
      if (error) throw new Error("failed to patch catalogue entry", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
    onMutate: async ({ id, patch }): Promise<PatchContext> => {
      const key = catalogueEntryKey(id);
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<CatalogueEntry>(key);
      if (previous) {
        // Optimistic update: apply only the fields that actually live on
        // CatalogueEntryDto. `revert_to_auto` is a PatchRequest-only control
        // field (a list of PascalCase field names to reset to automatic
        // enrichment) — it must not leak onto the cached entry. We leave
        // `operator_edited_fields` alone here and rely on the server echo in
        // `onSuccess` to deliver the authoritative lock state; the extra
        // one-render lag on that single chip is preferable to duplicating
        // the server's PascalCase field-lock bookkeeping on the client.
        const { revert_to_auto: _revertToAuto, ...applicable } = patch;
        queryClient.setQueryData<CatalogueEntry>(key, {
          ...previous,
          ...applicable,
        });
      }
      return { previous };
    },
    onError: (_error, { id }, context) => {
      if (context?.previous) {
        queryClient.setQueryData(catalogueEntryKey(id), context.previous);
      }
    },
    onSuccess: (data, { id }) => {
      queryClient.setQueryData(catalogueEntryKey(id), data);
      queryClient.invalidateQueries({ queryKey: CATALOGUE_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_MAP_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
    },
  });
}

export function useDeleteCatalogueEntry(): UseMutationResult<void, Error, string> {
  const queryClient = useQueryClient();
  return useMutation<void, Error, string>({
    mutationFn: async (id): Promise<void> => {
      const { error } = await api.DELETE("/api/catalogue/{id}", {
        params: { path: { id } },
      });
      if (error) throw new Error("failed to delete catalogue entry", { cause: error });
    },
    onSuccess: (_data, id) => {
      queryClient.removeQueries({ queryKey: catalogueEntryKey(id) });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_MAP_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
    },
  });
}

export function useReenrichOne(): UseMutationResult<void, Error, string> {
  const queryClient = useQueryClient();
  return useMutation<void, Error, string>({
    mutationFn: async (id): Promise<void> => {
      const { error } = await api.POST("/api/catalogue/{id}/reenrich", {
        params: { path: { id } },
      });
      if (error) throw new Error("failed to re-enrich catalogue entry", { cause: error });
    },
    onSuccess: (_data, id) => {
      queryClient.invalidateQueries({ queryKey: catalogueEntryKey(id) });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_MAP_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
    },
  });
}

export function useReenrichMany(): UseMutationResult<void, Error, CatalogueBulkReenrichRequest> {
  const queryClient = useQueryClient();
  return useMutation<void, Error, CatalogueBulkReenrichRequest>({
    mutationFn: async (body): Promise<void> => {
      const { error } = await api.POST("/api/catalogue/reenrich", { body });
      if (error)
        throw new Error("failed to re-enrich catalogue entries", {
          cause: error,
        });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: CATALOGUE_LIST_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_MAP_KEY });
      queryClient.invalidateQueries({ queryKey: CATALOGUE_FACETS_KEY });
    },
  });
}
