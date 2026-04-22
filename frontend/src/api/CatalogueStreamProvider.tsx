import type { ReactNode } from "react";
import { useCatalogueStream } from "@/api/hooks/catalogue-stream";

/**
 * Mounts the catalogue SSE subscription once for the entire authenticated
 * subtree. The hook patches `CATALOGUE_LIST_KEY`, `CATALOGUE_MAP_KEY`,
 * `CATALOGUE_FACETS_KEY`, and per-entry caches on every catalogue event,
 * so any page that reads catalogue-derived data via TanStack Query (the
 * catalogue page, the campaign composer, campaign detail, the history
 * pair page, and any future consumer) sees enrichment updates without
 * having to subscribe individually.
 *
 * This component must live inside `QueryClientProvider` (the hook calls
 * `useQueryClient` to invalidate caches) and inside the auth-protected
 * subtree (the `/api/catalogue/stream` endpoint requires a session
 * cookie — mounting it on the login page would 401-loop).
 */
interface CatalogueStreamProviderProps {
  children: ReactNode;
}

export default function CatalogueStreamProvider({ children }: CatalogueStreamProviderProps) {
  useCatalogueStream();
  return <>{children}</>;
}
