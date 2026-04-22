import {
  type HostnameValue,
  useIpHostnameContext,
} from "@/components/ip-hostname/IpHostnameProvider";

/**
 * Read the hostname for a single IP from the shared provider.
 *
 * Returns:
 * - `string` — positive cache hit.
 * - `null` — confirmed negative (no PTR record).
 * - `undefined` — cold/unknown; the IP has not been seeded or
 *   stream-delivered yet. Callers typically render the bare IP in this
 *   case and wait for the SSE event to arrive.
 *
 * Callers that need many IPs at once should prefer `useIpHostnames` so
 * the provider's single map identity flows through one `useMemo` instead
 * of dozens of individual subscriptions.
 */
export function useIpHostname(ip: string): HostnameValue | undefined {
  const { map } = useIpHostnameContext();
  return map.get(ip);
}
