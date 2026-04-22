import { useCallback } from "react";

/**
 * POST `/api/hostnames/:ip/refresh` to force a fresh PTR lookup.
 *
 * Returns a stable `(ip) => Promise<void>` that resolves on a 2xx and
 * rejects with a descriptive `Error` on any other status. The resolved
 * hostname does NOT come back in the HTTP response — the service
 * returns 202 and the new value arrives on the `/api/hostnames/stream`
 * SSE channel, which the shared provider reconciles into its map.
 *
 * No TanStack mutation wrapper: there is no client cache to invalidate.
 * The caller typically toasts on rejection (e.g. 429 rate-limit) and
 * ignores the resolved value — the provider's SSE handler is the single
 * source of truth for the new hostname.
 */
export function useRefreshHostname(): (ip: string) => Promise<void> {
  return useCallback(async (ip: string): Promise<void> => {
    // Encode so IPv6 colons and any unusual characters survive the URL
    // boundary. `encodeURIComponent` is correct here because axum's path
    // segment matcher decodes once on arrival.
    const encoded = encodeURIComponent(ip);
    const response = await fetch(`/api/hostnames/${encoded}/refresh`, {
      method: "POST",
      credentials: "include",
      headers: { Accept: "application/json" },
    });
    if (!response.ok) {
      // Surface the status so the caller's catch can decide between a
      // specific toast (429: "too many refreshes") and a generic one.
      throw new Error(`hostname refresh failed: HTTP ${response.status}`);
    }
  }, []);
}
