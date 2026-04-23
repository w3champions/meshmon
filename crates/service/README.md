# meshmon-service

The HTTP service half of meshmon. Receives agent pushes, persists them to VictoriaMetrics + Postgres, exposes operator-facing APIs, and serves the embedded React SPA.

## Embedded frontend

The release binary embeds `frontend/dist/` via the
[`memory-serve`](https://crates.io/crates/memory-serve) crate:

- `/`, `/index.html`, and any non-backend path serve the SPA's
  `index.html` with `Cache-Control: no-cache, no-store, must-revalidate`
  so deploys take effect on next navigation.
- Hashed asset paths (e.g. `/assets/index-<hash>.js`) get
  `Cache-Control: max-age=31536000, immutable`, an ETag, and — where the
  client accepts it — a pre-compressed brotli or gzip body baked into the
  binary at build time.
- Any `/api/*` path that isn't a registered handler returns **404** via a
  guard route, so the SPA fallback cannot shadow genuine backend misses.
  `/healthz`, `/readyz`, and `/metrics` are exact routes and are likewise
  safe.

### Dev mode vs release mode

In debug builds (`cargo run`, `cargo test`), `memory-serve` reads assets
from disk at request time — edit the frontend, rerun `npm run build`,
and the new assets are served without rebuilding the Rust binary.

In release builds (`cargo build --release`), assets are embedded and
pre-compressed into the binary.

### Building a release binary with the real frontend

```bash
# One-shot helper:
./scripts/build-release.sh

# …or manually:
cd frontend && npm ci && npm run build && cd ..
cargo build --release -p meshmon-service
```

Without a prior `npm run build`, `crates/service/build.rs` seeds a
minimal `frontend/dist/index.html` placeholder so `cargo check` /
`cargo test` run without Node.js. CI's `release-binary` job runs the
frontend build first, then the release `cargo build`, then uploads the
resulting binary as an artifact.

## Layout

- `src/main.rs` — binary entry: config load, DB connect, listener bind, ingestion spawn, graceful shutdown.
- `src/config.rs` — `meshmon.toml` parser + validation.
- `src/db.rs` — Postgres pool + migrations runner + TimescaleDB setup.
- `src/http/` — axum router and handlers (auth, health, OpenAPI, future user/agent APIs).
- `src/ingestion/` — data plane (validator, VM writer, PG writer, last-seen updater).
- `migrations/` — sqlx migrations.

## Ingestion pipeline

`crate::ingestion` houses the data plane:

- `validator` — pure functions that range-check incoming Protobuf payloads.
- `vm_writer` — batches Prometheus remote-write samples, snappy-encodes the protobuf body, POSTs to VictoriaMetrics with retry/backoff. Sample types come from the [`prometheus-reqwest-remote-write`](https://crates.io/crates/prometheus-reqwest-remote-write) crate (no vendored proto).
- `pg_writer` — inserts route snapshots into `route_snapshots` with the JSONB shapes defined in `ingestion::json_shapes`.
- `last_seen` — debounced `agents.last_seen_at` updater (30s window).
- `queue` — drop-oldest bounded queue primitive used for buffering during downstream outages.

Producers call `IngestionPipeline::push_metrics` / `push_snapshot` after the HTTP handler strips auth and decodes Protobuf. Workers run under the shared cancellation token and drain on shutdown.

Self-metrics are recorded via the `metrics` crate macros. A future task will wire the Prometheus exporter to serve them at `/metrics`.

## Tests

Integration tests live under `tests/`. They share a single `timescale/timescaledb` container per test binary (see `tests/common/mod.rs`).

`DATABASE_URL` overrides the container — useful for iterating against a long-lived local Postgres.

```bash
# Run the full suite:
cargo test --workspace

# Or against a local DB:
DATABASE_URL=postgres://postgres:meshmon@localhost/postgres \
    cargo test --workspace
```

## sqlx compile-time-checked queries

The service uses `sqlx::query!` / `sqlx::query_as!` macros. The macros validate SQL against either a live `DATABASE_URL` or the committed `.sqlx/` offline cache.

### Workflow

```bash
# One-time setup:
cargo install sqlx-cli --no-default-features --features rustls,postgres --version ~0.8

# After changing any query!/query_as! macro:
cargo xtask sqlx-prepare
git add .sqlx
```

`cargo xtask sqlx-prepare` spawns a throwaway `meshmon-sqlx-prep-<uuid>`
TimescaleDB container on a kernel-assigned host port, applies migrations,
runs `cargo sqlx prepare --workspace -- --all-targets --all-features`,
then tears the container down. Concurrent invocations are safe.

Without `DATABASE_URL`, set `SQLX_OFFLINE=true` (CI will).

## Agent registry

`meshmon_service::registry::AgentRegistry` keeps an in-memory snapshot of
the `agents` table. It refreshes every
`[agents].refresh_interval_seconds` (default 10 s) or on explicit
`force_refresh()`. The latter is the seam for the agent-register handler
to invoke after writing a new row, so the new agent is visible to
`/api/agent/targets` without waiting for the next tick.

`[agents].target_active_window_minutes` (default 5) controls which agents
appear in `active_targets()` results. The window is passed as a `Duration`
argument to `RegistrySnapshot::active_targets` at call time; handlers read
it from `state.registry.active_window()`, which was set at construction.

**Configuration knobs are read at startup.** Both `refresh_interval_seconds`
and `target_active_window_minutes` are captured when `AgentRegistry::new` is
called. SIGHUP-driven config reload updates `AppState::config` but does not
re-apply those values to a running registry; a service restart is required to
change refresh cadence or active-window.

**Resilience:** initial load at startup is fail-fast. After that, any
refresh that errors keeps the previous snapshot in place, emits
`meshmon_service_registry_refresh_errors_total`, and retries on the next
tick. Ingestion source validation therefore continues to accept known
agents during brief DB outages.

## Hostname resolution

The service owns an authoritative IP → hostname cache at
`crates/service/src/hostname/`, backed by a TimescaleDB hypertable
with a 90-day retention policy. Reverse-DNS lookups run through
`hickory-resolver` with single-flight dedup and panic containment;
completion events fan out over `/api/hostnames/stream` scoped to
the session that caused the lookup. Handlers returning
IP-carrying DTOs consume the cache via `hostname::hostnames_for`
and enqueue cold misses through `AppState::hostname_resolver`. See
`src/hostname/README.md` for the full contract.

Every IP-carrying response DTO ships a resolved hostname. DTOs with
a single IP carry `hostname: Option<String>`; flat-keyed shapes with
two IPs carry `source_hostname` / `target_hostname` (alerts), and
route-pair or campaign shapes carry `destination_hostname` alongside
hop-level `hostname` fields. All fields are stamped server-side by the
shared `stamp_hostnames` helper via one batched `hostnames_for` call
per response: positive hits set `Some(h)`, negative hits leave `None`,
cold misses enqueue a background resolution against the caller's
`SessionId`. Every field is `#[serde(skip_serializing_if =
"Option::is_none")]` — absent on the wire rather than `null` — and
no hostname state is written to persistent storage.

The frontend `frontend/src/components/ip-hostname/` module is the
sole consumer of these wire surfaces. It maintains a client-side
`Map<ip, Option<string>>` seeded from DTO hostname fields via the
`useSeedHostnamesOnResponse` hook, subscribes to
`/api/hostnames/stream` through an `EventSource` to receive
resolution completions, and calls `POST /api/hostnames/:ip/refresh`
from the operator-facing refresh action. All render sites read from
the module through `<IpHostname />` and the `useIpHostname` /
`useIpHostnames` hooks; no other code path touches the stream, the
refresh endpoint, or hostname DTO fields directly (see
`frontend/src/components/ip-hostname/README.md` for the full
render-site list and sanctioned exceptions).

## Agent API (gRPC)

The service exposes a tonic gRPC endpoint named `meshmon.AgentApi`. It shares
the same TCP port and listener as the REST API; the `auto::Builder` in
`main.rs` dispatches HTTP/1.1 (REST) and HTTP/2 (gRPC) on the same socket.

### RPCs

| RPC | Direction | Description |
|-----|-----------|-------------|
| `Register` | Agent → Service | Upserts the agent row in Postgres and force-refreshes the in-memory registry. Validates that the claimed IP matches the connection IP (loopback-exempt). |
| `PushMetrics` | Agent → Service | Accepts a `MetricsBatch` and enqueues it for ingestion into VictoriaMetrics + Postgres. Source agent must be registered. |
| `PushRouteSnapshot` | Agent → Service | Accepts a `RouteSnapshotRequest` and enqueues it for Postgres ingestion. Source agent must be registered. |
| `GetConfig` | Service → Agent | Returns the current `[probing]` configuration (enabled protocols, rate table, all thresholds). Reloaded on SIGHUP. |
| `GetTargets` | Service → Agent | Returns the list of active agents (within `[agents].target_active_window_minutes`) excluding the caller. |

### Auth

Every RPC requires a `Authorization: Bearer <token>` gRPC metadata header.
The token is the `[agent_api].shared_token` / `shared_token_env` value. If
the token is unset, all RPCs return `UNAVAILABLE`. Wrong or missing tokens
return `UNAUTHENTICATED`.

### Rate limit

A per-IP token-bucket rate limit is applied before any RPC reaches the handler:

- **`rate_limit_per_minute`** (default 60) — sustained requests per minute.
- **`rate_limit_burst`** (default 30) — burst absorbed instantly; sized for
  the three startup RPCs (Register + GetConfig + GetTargets) across a fleet
  of agents sharing a proxy egress IP.

Requests that exceed the limit receive HTTP 429 before the gRPC layer sees them.

### Error mapping

| Condition | gRPC status |
|-----------|-------------|
| Token missing | `UNAUTHENTICATED` |
| Token mismatch | `UNAUTHENTICATED` |
| Agent API not configured (no token) | `UNAVAILABLE` |
| Unknown source agent on push | `PERMISSION_DENIED` |
| Claimed IP ≠ connection IP | `PERMISSION_DENIED` |
| Agent ID already registered with a different IP | `ALREADY_EXISTS` |
| Invalid payload (empty IDs, bad IP bytes, out-of-range values) | `INVALID_ARGUMENT` |
| Database or internal error | `INTERNAL` |

### Deployment modes

**Behind a reverse proxy (recommended)**

The proxy terminates TLS (including HTTP/2 ALPN negotiation) and forwards
plaintext gRPC to the service via `grpc_pass`. Leave `[agent_api.tls]`
commented out; the service binds HTTP/2 cleartext on `[service].listen_addr`.
Set `[service].trust_forwarded_headers = true` if the proxy sets
`X-Forwarded-For` so per-IP rate limiting uses the original client address.

**Standalone TLS (no proxy)**

Uncomment `[agent_api.tls]` and provide `cert_path` / `key_path`. The
service loads the certificate chain at startup and re-reads it on SIGHUP
(zero-downtime certificate rotation). Agents connect directly with TLS.

### Debugging with grpcurl

```bash
# List available RPCs (requires server reflection or a local .proto):
grpcurl -plaintext localhost:8080 list meshmon.AgentApi

# Call GetConfig (no request body needed):
grpcurl -plaintext \
  -H 'Authorization: Bearer <your-token>' \
  -d '{}' \
  localhost:8080 meshmon.AgentApi/GetConfig
```

## Configuration

See `meshmon.toml` (canonical form lives in the deploy/ example). Secrets go through `*_env` indirection.

### `[probing]` — UDP probe secret

The `[probing]` section requires a UDP probe secret — exactly 8 bytes,
encoded as `hex:` or `base64:`. Agents embed it in their UDP probe
packets; the UDP echo listener drops traffic that does not match.
Rotate by setting `udp_probe_previous_secret` (or `_env`) to the
outgoing value; listeners accept either during the rotation window.

Provide inline *or* via env indirection — mutually exclusive. The env
form keeps the secret out of the committed config and is the
quick-start default; compose injects `MESHMON_UDP_PROBE_SECRET` from
`deploy/.env`.

```toml
[probing]
# Env form (quick-start default):
udp_probe_secret_env = "MESHMON_UDP_PROBE_SECRET"
# udp_probe_previous_secret_env = "MESHMON_UDP_PROBE_PREVIOUS_SECRET"

# Inline form (alternative — exactly 8 bytes, hex: or base64: prefix):
# udp_probe_secret = "hex:0011223344556677"
# udp_probe_previous_secret = "hex:ffeeddccbbaa9988"
```

## Observability

- `GET /healthz` — liveness. 200 while the process is up. No auth.
- `GET /readyz` — readiness. 200 when startup completes; 503 during
  shutdown drain. No auth.
- `GET /metrics` — Prometheus text-format self-metrics. Optional
  Basic auth (see below).

### Basic auth for `/metrics`

Opt-in via `[service.metrics_auth]`:

```toml
[service.metrics_auth]
username = "prometheus"
password_hash_env = "MESHMON_METRICS_PASSWORD_HASH"   # or password_hash = "..."
```

Omit the section → `/metrics` is ungated. When configured:
- Request without credentials → `401 Unauthorized` with
  `WWW-Authenticate: Basic realm="meshmon metrics"`.
- Wrong creds → `401`.
- Password comparison is argon2 PHC — constant-time username compare
  plus argon2 verify.

The scraping Prometheus needs matching credentials:

```yaml
scrape_configs:
  - job_name: meshmon-service
    basic_auth:
      username: prometheus
      password: <plaintext>
    static_configs:
      - targets: [ '<host>:8080' ]
```

### Metric catalog

- `meshmon_service_uptime_seconds` — seconds since process start.
- `meshmon_service_build_info{version,commit}` — singleton gauge = 1.
- `meshmon_service_http_requests_total{method,endpoint,status}` —
  request counts keyed on axum matched-route templates. Via
  `axum-prometheus`.
- `meshmon_service_http_requests_duration_seconds{method,endpoint,status}`
  — latency histogram. Via `axum-prometheus`.
- `meshmon_service_http_requests_pending{method,endpoint}` — in-flight
  gauge. Via `axum-prometheus`.
- `meshmon_service_ingest_batches_total{outcome}` — `ok` or `write_error`.
- `meshmon_service_ingest_samples_total` — samples shipped to VM.
- `meshmon_service_ingest_dropped_total{source}` — source ∈
  `{metrics, snapshot, touch}`.
- `meshmon_service_vm_write_duration_seconds` — VM remote-write latency.
- `meshmon_service_pg_snapshot_duration_seconds` — route-snapshot INSERT
  latency.
- `meshmon_service_last_seen_writes_total` — successful
  `agents.last_seen_at` updates.
- `meshmon_service_registry_agents{state}` — `active` or `stale`.
- `meshmon_service_registry_last_refresh_age_seconds` — age of the
  registry snapshot.
- `meshmon_service_registry_refresh_errors_total` — failed refreshes.

### Registry-derived gauges

Appended at scrape time from the live registry snapshot (deregistered
agents disappear on the next scrape):

- `meshmon_agent_info{source,agent_version}` — gauge = 1 per known agent.
- `meshmon_agent_last_seen_seconds{source}` — Unix timestamp of last push.
