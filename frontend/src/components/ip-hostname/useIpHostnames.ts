import { useMemo, useRef } from "react";
import {
  type HostnameValue,
  useIpHostnameContext,
} from "@/components/ip-hostname/IpHostnameProvider";

/**
 * Bulk hostname lookup.
 *
 * Returns a `Map<ip, hostname | null | undefined>` whose identity is stable
 * so long as both the input IP set and the corresponding map values are
 * unchanged. Useful for virtualised tables that want one stable reference
 * to diff against on each render rather than N individual subscriptions.
 *
 * We serialise the (sortedIps, per-ip value) pair into a cheap `signature`
 * string and cache the built map alongside its signature on a ref. Every
 * render re-computes the current signature; when it matches the cached
 * one, the same `Map` instance comes back out, so downstream `useMemo`
 * consumers see stable identity across provider updates that only affect
 * IPs we don't track.
 *
 * ### Call-site requirement: stable `ips` identity
 *
 * The signature-based memo only pays off when `ips` keeps a stable
 * reference across renders. A new `["10.0.0.1", ...]` literal on every
 * render defeats the inner `useMemo([ips])` that sorts + dedupes the
 * set, which in turn rebuilds the signature and the result map even when
 * nothing meaningful changed. Hoist the array to module scope (for
 * fixed sets) or wrap it in `useMemo` at the call site.
 *
 * Small sets (< ~50 IPs) are cheap enough that a per-render rebuild is
 * not a performance concern in practice — the stability guidance matters
 * mainly for virtualised tables with hundreds of rows or for consumers
 * that key `useMemo` / `useEffect` deps off the returned map identity.
 *
 * @example
 * ```tsx
 * function AgentTable({ rows }: { rows: readonly AgentRow[] }) {
 *   // Stable IP list across renders so the hook's signature cache hits.
 *   const ips = useMemo(() => rows.map((r) => r.ip), [rows]);
 *   const hostnames = useIpHostnames(ips);
 *   // `hostnames` identity is stable until a tracked IP resolves.
 *   return <VirtualList rows={rows} hostnames={hostnames} />;
 * }
 * ```
 */
export function useIpHostnames(
  ips: readonly string[],
): ReadonlyMap<string, HostnameValue | undefined> {
  const { map } = useIpHostnameContext();

  // Deduplicate + sort so the cache key doesn't depend on call-site ordering.
  const sortedIps = useMemo(() => {
    const uniq = Array.from(new Set(ips));
    uniq.sort();
    return uniq;
  }, [ips]);

  // Build the signature + the result map in one pass. `value` is encoded
  // as `=foo` for positive, `-` for negative (`null`), `?` for cold miss.
  // Cache the pair behind a ref so successive renders that produce the
  // same signature hand back the same Map instance.
  const cache = useRef<{
    signature: string;
    result: ReadonlyMap<string, HostnameValue | undefined>;
  } | null>(null);

  const parts: string[] = [];
  const out = new Map<string, HostnameValue | undefined>();
  for (const ip of sortedIps) {
    if (!map.has(ip)) {
      parts.push(`${ip}?`);
      out.set(ip, undefined);
      continue;
    }
    const value = map.get(ip);
    if (value === null) {
      parts.push(`${ip}-`);
      out.set(ip, null);
    } else {
      parts.push(`${ip}=${value}`);
      out.set(ip, value as HostnameValue);
    }
  }
  const signature = parts.join("|");

  if (cache.current !== null && cache.current.signature === signature) {
    return cache.current.result;
  }
  cache.current = { signature, result: out };
  return out;
}
