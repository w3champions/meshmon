import { useEffect, useRef } from "react";
import {
  type HostnameSeedEntry,
  useIpHostnameContext,
} from "@/components/ip-hostname/IpHostnameProvider";

/**
 * Seed the shared hostname map from a TanStack-Query response payload.
 *
 * Usage inside a query hook:
 *
 * ```ts
 * const query = useQuery({ queryKey: [...], queryFn });
 * useSeedHostnamesOnResponse(query.data, (d) => d.entries);
 * return query;
 * ```
 *
 * The `selector` is called inside a `useEffect` keyed on `data`, so it
 * runs once per response (not on every render). It MUST return an
 * iterable of `{ ip, hostname }` — typically the entries / rows array on
 * the response DTO. Hostname `undefined` is ignored by the provider so
 * negative-cache / cold-miss DTOs never blank out a resolved value.
 *
 * The selector is captured by a ref so inline arrow functions at call
 * sites don't force the effect to re-run every render. The contract is
 * "read from the supplied `data`", so `data` is the authoritative
 * trigger.
 */
export function useSeedHostnamesOnResponse<TData>(
  data: TData | undefined,
  selector: (data: TData) => Iterable<HostnameSeedEntry>,
): void {
  const { seedFromResponse } = useIpHostnameContext();

  const selectorRef = useRef(selector);
  selectorRef.current = selector;

  useEffect(() => {
    if (data === undefined || data === null) return;
    const entries = selectorRef.current(data);
    seedFromResponse(entries);
  }, [data, seedFromResponse]);
}
