# `components/ip-hostname/`

Shared rendering + plumbing for IP addresses with their reverse-DNS
hostname. Every frontend render site that needs "IP + optional hostname"
goes through this module — no bespoke rendering, no ad-hoc streams.

## Public surface

| Export | Purpose |
|---|---|
| `<IpHostnameProvider>` | Owns the shared `Map<ip, hostname \| null>` and the single `/api/hostnames/stream` EventSource. Mounted once per authenticated session. |
| `<IpHostname ip fallback?>` | Primary render primitive — `ip` with a muted `(hostname)` suffix once resolved. |
| `useIpHostname(ip)` | Hook variant for callers that own their own rendering (e.g. Cytoscape node labels). |
| `useIpHostnames(ips)` | Bulk variant returning a stable map reference keyed on the input IP set. |
| `useRefreshHostname()` | Returns a stable `(ip) => Promise<void>` that POSTs `/api/hostnames/:ip/refresh`. |
| `useSeedHostnamesOnResponse(data, selector)` | Primes the provider map from TanStack-Query response payloads. |
| `formatIpWithHostname`, `hostnameDisplay`, `tooltipForHostname` | Plain-text helpers for non-JSX contexts. |

## Usage rules

These are conventions — enforced by code review today; an ESLint rule
may follow if the conventions drift.

1. **Only this module reads the `hostname` DTO field.** Render sites do
   not touch `.hostname` on catalogue entries, agents, path DTOs, etc.
   They call `<IpHostname ip={row.ip} />` and let the module resolve.
2. **Only this module opens `/api/hostnames/stream`.** No component
   outside `components/ip-hostname/` should construct an `EventSource`
   pointed at that URL.
3. **Only this module calls `POST /api/hostnames/:ip/refresh`.** Render
   sites that expose a refresh affordance (e.g. the catalogue drawer)
   invoke `useRefreshHostname()`; they don't hand-roll a `fetch` call.
4. **Every DTO-returning TanStack query that carries IPs seeds the
   provider.** The query hook calls
   `useSeedHostnamesOnResponse(query.data, (d) => d.entries)` (or the
   equivalent selector) so later renders of the same IP on another page
   don't flicker while the SSE event re-arrives.

## Lifecycle

The provider is mounted inside `AppShell` — that is, inside the
auth-gated subtree, so the session cookie is in place before the
EventSource opens, and inside `QueryClientProvider` so seed hooks can
call `useIpHostnameContext()` from any query hook.

```text
<QueryClientProvider>
  <RouterProvider>
    // auth route →
    <AppShell>
      <CatalogueStreamProvider>
        <IpHostnameProvider>
          <Outlet />
        </IpHostnameProvider>
      </CatalogueStreamProvider>
    </AppShell>
  </RouterProvider>
</QueryClientProvider>
```

The EventSource opens on provider mount and closes on unmount. There is
never more than one instance per session. Transport errors recover via
the browser's native `EventSource` reconnect — no retry banner, no
toast, no exponential-backoff wrapper.

## State semantics

| Provider map state | Meaning | Render |
|---|---|---|
| key absent | Cold miss — the IP hasn't been seeded or streamed yet. | Bare IP. |
| value `null` | Confirmed negative — no PTR record. | Bare IP. |
| value `string` | Positive cache hit. | `ip (hostname)`. |

`seedFromResponse` skips entries whose `hostname` is `undefined`
(differentiating "not present on this DTO" from an intentional negative)
so a cold-miss response can't overwrite a positive value that arrived
earlier via SSE.
