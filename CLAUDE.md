# meshmon

Continuous mesh-network monitoring system. Agents probe every other node;
a central service ingests, stores, and alerts on regressions and route changes.

## Workspace layout

| Crate | Role |
|-------|------|
| `crates/service` | Central axum service: API, ingestion, alerting |
| `crates/agent` | Per-node probe agent (tokio + tonic gRPC client) |
| `crates/protocol` | Shared protobuf types (`meshmon.proto`, tonic codegen) |
| `crates/revtunnel` | Reverse-tunnel transport (yamux inside a tonic bidi stream) |
| `crates/common` | Shared utilities |
| `frontend/` | React 19 + Tailwind SPA, embedded into the service binary |

## Build and test

```bash
cargo build --workspace
cargo test --workspace --all-targets
cargo clippy --workspace -- -D warnings
```

Service integration tests require Docker (TimescaleDB via `testcontainers`).

The release service binary embeds the React SPA via `memory-serve`.
Produce a deployable binary with:

```bash
./scripts/build-release.sh
```

`cargo build` alone uses a placeholder `index.html` synthesized by
`crates/service/build.rs` so backend-only dev flows don't need Node.js.

## Database

Postgres + TimescaleDB. Migrations in `crates/service/migrations/`.
Compile-time-checked queries via `sqlx::query!` with a committed `.sqlx/`
offline cache. After changing any query macro, regenerate the cache (see
README for steps).

## Agent

Env-var-configured, gRPC-based agent. See README "Running the agent" for
the full variable table and lifecycle description.

Key patterns:
- `ServiceApi` trait abstracts gRPC for testability (generic, not dyn)
- `AgentRuntime<A: ServiceApi>` owns supervisors, config broadcast, cancel token
- Retry with exponential backoff + jitter for all bootstrap RPCs
- `CancellationToken` tree for graceful shutdown propagation
- UDP prober uses shared-socket dispatcher pool (`UdpProberPool`) rather
  than per-target sockets — O(1) fd count as targets grow
- Trippy driver uses `spawn_blocking` + a global `Semaphore`
  (`MESHMON_ICMP_TARGET_CONCURRENCY`, default 32) to cap raw-socket + thread use
- Dedicated ICMP pinger (`surge-ping`, `probing/icmp.rs`) runs always-on alongside
  trippy so the state machine retains ICMP samples even when the primary protocol
  swings to TCP or UDP; requires `CAP_NET_RAW`
- `TargetStateMachine` (in `state.rs`) evaluates per-protocol health and derives
  path health every 10 s; the supervisor publishes resulting rates and window
  sizes to the four prober watch channels — probers are never respawned
- Agents run TCP + UDP echo listeners on `MESHMON_TCP_PROBE_PORT` /
  `MESHMON_UDP_PROBE_PORT`. Both listeners (and the `UdpProberPool`'s
  shared sender socket) bind `[::]` dual-stack (`IPV6_V6ONLY=false`) so
  a single socket serves both IPv4 and IPv6 peers; the receiver paths
  normalize v4-mapped-v6 peer addresses via `IpAddr::to_canonical()`
  before allowlist / dispatch-map lookups. UDP is secret-gated (8-byte
  secret from `ConfigResponse`) + allowlist-gated (IPs from
  `GetTargets`).
- Reverse tunnel (`tunnel.rs`) keeps one long-lived `OpenTunnel` RPC open
  so the service can invoke `AgentCommand::RefreshConfig` through it —
  cuts config-fetch latency from up-to-5min (poll) to near-immediate.
  Reconnects with 1s→60s exponential backoff + ±25% jitter on termination.

## Service

Key patterns:
- `TunnelManager` (from `meshmon-revtunnel`) tracks one `tonic::Channel`
  per registered agent tunnel. `commands::spawn_config_watcher` fans out
  concurrent `AgentCommand::RefreshConfig` calls across the registry on
  every SIGHUP-driven config reload; per-call deadline 10s, failures
  logged and counted, no retries (the agent's 5-min poll is the safety net).
- `TunnelManager::close_all` cancels every driver token on shutdown so
  outer response streams EOF and the HTTP/2 conn drain completes within
  `shutdown_deadline`.
- Self-metrics: `meshmon_service_tunnel_agents` (gauge — registered
  tunnels) and `meshmon_service_command_rpcs_total{method,outcome}`
  (counter — fan-out RPC outcomes).

## Conventions

- Squash-merge only (no merge commits, no rebase)
- `cargo fmt` + `cargo clippy -- -D warnings` must pass
- Test coverage: unit tests in `#[cfg(test)]` modules, integration tests in `tests/`
