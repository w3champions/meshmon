import {
  type HostnameSeedEntry,
  useIpHostnameContext,
} from "@/components/ip-hostname/IpHostnameProvider";

/**
 * Return the stable `seedFromResponse` callback from the shared provider.
 *
 * Companion to {@link useSeedHostnamesOnResponse}: the query-hook helper
 * captures a response and an effect, whereas this hook hands back the
 * raw seed callback so callers that own their own effect (or tests that
 * drive the provider synchronously) can prime the map directly.
 *
 * Prefer {@link useSeedHostnamesOnResponse} for TanStack-Query flows —
 * this hook exists to keep the provider's internal
 * `useIpHostnameContext` out of the public barrel while still letting
 * tests and ad-hoc consumers seed the shared map without hand-rolling
 * a context accessor.
 */
export function useSeedHostnames(): (entries: Iterable<HostnameSeedEntry>) => void {
  return useIpHostnameContext().seedFromResponse;
}
