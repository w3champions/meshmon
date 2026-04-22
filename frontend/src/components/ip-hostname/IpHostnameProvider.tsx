import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

/**
 * Value carried in the shared hostname map.
 *
 * - `string` — positive cache hit (reverse-DNS resolved).
 * - `null` — confirmed negative (no PTR record).
 * - `undefined` is represented by map-miss (the key is absent); consumers
 *   treat missing keys as "cold / unknown" and render the bare IP.
 */
export type HostnameValue = string | null;

/** Entry shape accepted by {@link IpHostnameContextValue.seedFromResponse}. */
export interface HostnameSeedEntry {
  ip: string;
  hostname?: string | null | undefined;
}

export interface IpHostnameContextValue {
  /**
   * Shared IP → hostname map. Immutable: the provider swaps to a new
   * `Map` instance on every update so consumers that key memoisation on
   * the map identity see a stable reference between renders.
   */
  map: ReadonlyMap<string, HostnameValue>;
  /**
   * Prime the map from a response payload. `undefined` hostnames are
   * ignored so a freshly returned DTO that omits `hostname` (cold-miss or
   * negative, depending on handler semantics) does not overwrite a value
   * the SSE stream already resolved. The callback is stable across
   * renders — safe to include in a `useEffect` dependency list.
   */
  seedFromResponse: (entries: Iterable<HostnameSeedEntry>) => void;
}

const IpHostnameContext = createContext<IpHostnameContextValue | null>(null);

/** Endpoint backing the SSE subscription. Kept here so the test harness can mock a single URL. */
export const HOSTNAME_STREAM_URL = "/api/hostnames/stream";

/** Backend SSE event name; must match `Event::default().event(...)` in `hostname/handlers.rs`. */
const HOSTNAME_RESOLVED_EVENT = "hostname_resolved";

/**
 * Wire payload for a single `hostname_resolved` SSE frame.
 *
 * Mirrors `components["schemas"]["HostnameEvent"]` in the generated OpenAPI
 * types. Kept local so this provider module doesn't pull a larger generated
 * type graph in just for one SSE payload.
 */
interface HostnameResolvedFrame {
  ip: string;
  hostname?: string | null;
}

function isHostnameResolvedFrame(value: unknown): value is HostnameResolvedFrame {
  if (typeof value !== "object" || value === null) return false;
  const ip = (value as { ip?: unknown }).ip;
  if (typeof ip !== "string" || ip.length === 0) return false;
  const hostname = (value as { hostname?: unknown }).hostname;
  return hostname === undefined || hostname === null || typeof hostname === "string";
}

interface IpHostnameProviderProps {
  children: ReactNode;
}

/**
 * Owns the shared IP → hostname map and the single EventSource backing
 * the `/api/hostnames/stream` subscription.
 *
 * Mount this once, inside `QueryClientProvider` but inside the auth-gated
 * subtree (the backend endpoint requires a session cookie). Every
 * `<IpHostname>` / `useIpHostname` call resolves through this provider;
 * there is no per-consumer EventSource.
 *
 * Error handling: on SSE transport failure we silently keep whatever the
 * map already holds. The browser's native `EventSource` reconnect loop
 * re-establishes the stream when the service is back.
 */
export function IpHostnameProvider({ children }: IpHostnameProviderProps) {
  const [map, setMap] = useState<ReadonlyMap<string, HostnameValue>>(() => new Map());

  // Keep a ref on the latest map so the merge callback stays referentially
  // stable regardless of re-render — callers that include `seedFromResponse`
  // in a `useEffect` dependency list shouldn't re-run every render.
  const mapRef = useRef(map);
  mapRef.current = map;

  const applyUpdates = useCallback((updates: Iterable<readonly [string, HostnameValue]>) => {
    let changed = false;
    const next = new Map(mapRef.current);
    for (const [ip, hostname] of updates) {
      if (!next.has(ip) || next.get(ip) !== hostname) {
        next.set(ip, hostname);
        changed = true;
      }
    }
    // Skip the setState round-trip when nothing changed — keeps noisy
    // re-seed calls cheap and avoids redundant re-renders downstream.
    if (!changed) return;
    mapRef.current = next;
    setMap(next);
  }, []);

  const seedFromResponse = useCallback(
    (entries: Iterable<HostnameSeedEntry>) => {
      const updates: Array<readonly [string, HostnameValue]> = [];
      for (const entry of entries) {
        // Skip `undefined`: an undefined hostname on a DTO means the handler
        // couldn't resolve it synchronously (cold miss). The SSE event will
        // deliver the real value; overwriting a known entry with `undefined`
        // would blank out a positive hit.
        if (entry.hostname === undefined) continue;
        if (typeof entry.ip !== "string" || entry.ip.length === 0) continue;
        updates.push([entry.ip, entry.hostname]);
      }
      if (updates.length === 0) return;
      applyUpdates(updates);
    },
    [applyUpdates],
  );

  // Open the EventSource once on mount, close on unmount. The backend
  // emits `Event::default().event("hostname_resolved")`, so we attach a
  // named listener rather than `onmessage` (the default-event stream is
  // empty on this endpoint).
  useEffect(() => {
    // `EventSource` is a browser global; under jsdom the test setup stubs
    // it. If it's genuinely missing at runtime (SSR, an exotic embed
    // without EventSource), fall back to seed-only behaviour — render
    // sites still show IPs and the seed path still primes the map.
    if (typeof EventSource === "undefined") return;

    const source = new EventSource(HOSTNAME_STREAM_URL);
    const handleResolved = (event: MessageEvent<string>) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(event.data);
      } catch {
        // Malformed frame — skip and keep the stream alive.
        return;
      }
      if (!isHostnameResolvedFrame(parsed)) return;
      // Normalise `undefined` to `null` before storing so the map's value
      // domain stays `string | null` (the map-miss case is "key absent").
      const value: HostnameValue = parsed.hostname ?? null;
      applyUpdates([[parsed.ip, value]]);
    };
    source.addEventListener(HOSTNAME_RESOLVED_EVENT, handleResolved as EventListener);
    // Native EventSource reconnect handles transport-level failures. No
    // banner, no retry storm — silent recovery.

    return () => {
      source.removeEventListener(HOSTNAME_RESOLVED_EVENT, handleResolved as EventListener);
      source.close();
    };
  }, [applyUpdates]);

  const value = useMemo<IpHostnameContextValue>(
    () => ({ map, seedFromResponse }),
    [map, seedFromResponse],
  );

  return <IpHostnameContext.Provider value={value}>{children}</IpHostnameContext.Provider>;
}

/**
 * Internal accessor — consumers should use {@link useIpHostname},
 * {@link useIpHostnames}, or {@link useSeedHostnamesOnResponse} instead
 * of reading the raw context value. Exported for sibling hooks and the
 * test harness only; components outside `components/ip-hostname/` must
 * not import this directly.
 */
export function useIpHostnameContext(): IpHostnameContextValue {
  const ctx = useContext(IpHostnameContext);
  if (ctx === null) {
    throw new Error(
      "IpHostnameProvider missing. Wrap the subtree in <IpHostnameProvider>, " +
        "typically inside the auth-gated shell that composes your route tree.",
    );
  }
  return ctx;
}
