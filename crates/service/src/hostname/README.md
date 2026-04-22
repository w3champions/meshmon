# hostname

IP → hostname reverse-DNS cache.

## Storage

`ip_hostname_cache` — TimescaleDB hypertable, append-only, 7-day
chunks, 90-day retention policy (`add_retention_policy(INTERVAL '90
days')`). Positive hits: `hostname = <str>`; confirmed NXDOMAIN:
`hostname IS NULL`; transient resolver failures are never written.

Readers use `DISTINCT ON (ip) ORDER BY ip, resolved_at DESC` with a
`resolved_at > NOW() - INTERVAL '90 days'` freshness predicate; the
`(ip, resolved_at DESC)` index makes this an index-only scan.

Queries are runtime-typed (`sqlx::query_as`) rather than compile-time
`sqlx::query!` macros so the committed offline cache doesn't need
regenerating for this table. Revisit if the module grows enough
queries to make compile-time checking worthwhile.

## Resolver

Single `Resolver` task wrapping a `hickory-resolver` backend behind a
thin `ResolverBackend` trait. Tests inject a `StubHostnameBackend`
from `tests/common/mod.rs`. The resolver guarantees:

- Single-flight dedup per IP (`Mutex<HashMap<IpAddr, PendingLookup>>`).
- `Semaphore` cap on concurrent in-flight lookups
  (`[hostname_resolver].max_in_flight`, default 32).
- Panic containment via `AssertUnwindSafe(...).catch_unwind()` so a
  misbehaving backend can't permanently poison the pending map.
- Canonicalisation of IPv4-mapped IPv6 at the edge, so dual-stack
  views don't cache two rows per logical host.

## Session-scoped SSE

`HostnameBroadcaster` maintains a `DashMap<SessionId, mpsc::Sender>`
registry. Each pending-lookup record records the session IDs that
enqueued it; on completion the resolver fans the event out **only**
to those sessions. This diverges from `catalogue::events::CatalogueBroker`'s
`tokio::sync::broadcast` shape because hostname events must not leak
across sessions.

## Refresh

`POST /api/hostnames/:ip/refresh` bypasses cache freshness. Rate
limit: 60 calls / minute / session, enforced by a `DashMap` on
`AppState` (`HostnameRefreshLimiter`) with periodic idle-bucket
sweep.

## Consumer contract

Any handler that returns a DTO carrying IPs should:

1. Collect the set of IPs in the result.
2. Call `hostname::hostnames_for(pool, ips)` once in bulk.
3. Fill `hostname: Option<String>` on the DTO from the map.
4. For cold-cache IPs (key absent), enqueue a lookup:
   `state.hostname_resolver.enqueue(ip, session_id).await`.
5. Return. Do not await lookup completion — the SSE stream delivers
   the late-arriving values.

## HTTP surface

- `GET /api/hostnames/stream` — authenticated SSE, session-scoped.
  Emits `hostname_resolved { ip, hostname }` events only for
  lookups this session's requests initiated.
- `POST /api/hostnames/:ip/refresh` — 202 on accept, 429 over the
  60/min/session cap.

## Configuration

```toml
[hostname_resolver]
upstreams     = []       # empty => use the host's resolv.conf
timeout_ms    = 3000
max_in_flight = 32
```

## Module layout

```
hostname/
├── backend.rs         ResolverBackend trait + HickoryBackend
├── handlers.rs        hostname_stream + hostname_refresh (axum)
├── ip_canon.rs        canonicalize(IpAddr)
├── mod.rs             public surface (pub use ...)
├── README.md          this file
├── refresh_limit.rs   HostnameRefreshLimiter
├── repo.rs            hostnames_for + record_positive/_negative
├── resolver.rs        Resolver task
└── sse.rs             HostnameBroadcaster + SessionHandle
```
