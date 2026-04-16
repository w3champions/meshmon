# meshmon-service

The HTTP service half of meshmon. Receives agent pushes, persists them to VictoriaMetrics + Postgres, exposes operator-facing APIs, and serves the embedded React SPA.

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

# Bring up Postgres + apply migrations + regenerate the cache:
docker run -d --rm --name meshmon-prep -p 55432:5432 \
    -e POSTGRES_PASSWORD=meshmon timescale/timescaledb:2.26.3-pg16
sleep 3
export DATABASE_URL=postgres://postgres:meshmon@127.0.0.1:55432/postgres
sqlx migrate run --source crates/service/migrations
cargo sqlx prepare --workspace -- --all-targets --all-features
git add .sqlx
docker stop meshmon-prep
```

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

The `[probing]` section requires a `udp_probe_secret` — exactly 8 bytes,
encoded as `hex:` or `base64:`. Agents embed this secret in their UDP
probe packets; the UDP echo listener drops traffic that does not match
it. Rotate by setting `udp_probe_previous_secret` to the outgoing value;
listeners accept either during the rotation window.

```toml
[probing]
# Exactly 8 bytes, hex: or base64: prefix. Required.
udp_probe_secret = "hex:0011223344556677"
# Optional rotation companion — listeners accept either.
# udp_probe_previous_secret = "hex:ffeeddccbbaa9988"
```
