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

```sh
cargo build --workspace
cargo xtask test            # canonical test command — auto-provisions TimescaleDB and runs nextest
cargo xtask test-e2e        # end-to-end: brings up deploy/docker-compose.yml and runs cargo e2e
cargo xtask test-db down    # stop the shared test database when finished
cargo clippy --workspace -- -D warnings
```

`cargo test` still works as a zero-setup fallback (spawns a TimescaleDB container per integration-test binary via testcontainers). `cargo nextest run` directly is not supported — use `cargo xtask test`, which provisions a single shared Postgres and sets `DATABASE_URL` so every test connects to it. `cargo xtask test` excludes `xtask` and `meshmon-e2e` — run those via `cargo test -p xtask` and `cargo xtask test-e2e` respectively. See `crates/service/tests/common/mod.rs` for the three-tier isolation contract used by the test harness.

`deploy/docker-compose.yml` is the local-dev-safe compose file; `deploy/docker-compose.ci-cache.yml` is a CI-only overlay that adds the GHA buildx cache backend (requires `ACTIONS_RUNTIME_TOKEN`) and is wired in via `MESHMON_E2E_CACHE_OVERLAY` in the workflow — do not pass it locally.

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
- Each ICMP trippy round picks a unique non-zero 16-bit `trace_identifier`
  (monotonic `AtomicU16`, skip 0) so concurrent tracers on the same host
  stop cross-attributing each other's ICMP replies. TCP/UDP rounds leave
  the default — trippy matches those on port/address.
- After each round, hops are checked against the peer-IP allowlist
  (sourced from `GetTargets`). If any hop carries a sibling target's IP,
  the observation is discarded (not a timeout, not an error). A rate-
  limited `tracing::warn!` (once per 60 s per process) reports which
  sibling IP leaked.
- The route tracker retains silent TTLs as padded `HopSummary`s (empty
  `observed_ips`, `loss_pct = 1.0`) and truncates snapshots at the first
  position where the target's own IP appears. This matches mtr's output
  shape and stops trippy's over-probing from oscillating the reported
  hop count.
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
- `ip_catalogue` is the sole authority for IP geography, ASN, and
  network operator. `agents` keeps runtime fields only; the
  `agents_with_catalogue` view left-joins the two so agent-facing
  queries resolve geo without duplicating columns.
- Boot-time constraint: `[enrichment.ipgeolocation] enabled = true`
  requires `acknowledged_tos = true`. The config loader aborts
  startup otherwise.
- Campaign scheduler is a single tokio task, gated on `[campaigns]
  enabled` (default `false` until T45's real dispatcher lands — with
  the T44 `NoopDispatcher` active, pairs flip `pending → dispatched`
  but never settle). When enabled it subscribes to the
  `campaign_state_changed` Postgres NOTIFY channel (see
  `measurement_campaigns_notify` trigger) plus a periodic tick
  (default 500 ms) and issues fair-RR batches across active campaigns
  to a pluggable `PairDispatcher`. The NOTIFY channel name is a load-
  bearing contract — keep trigger + listener in lockstep on rename.

## Alerting

Alert rules and Alertmanager config live under `deploy/`:

- `deploy/alerts/rules.yaml` — VMAlert rules evaluated against
  VictoriaMetrics. Rule groups map to stable `category` labels
  consumed by the frontend alerts filter.
- `deploy/alertmanager/alertmanager.yml` — default routing with
  per-severity Discord receivers and an unreachable→loss inhibit rule.
- Discord webhook URLs are injected at container start via
  docker-compose's `secrets:` stanza with `environment:` source; see
  `deploy/docker-compose.yml`. Nothing touches the host filesystem.

Validate on every change:

```bash
cargo test -p meshmon-service --test alert_metrics_contract   # hermetic metric cross-check
cargo test -p meshmon-service --test alerts_validation        # integration (requires Docker)
cargo e2e                                                     # optional: end-to-end delivery smoke
```

See `deploy/alerts/README.md` for the label contract and editing workflow.

## Dashboards

Grafana dashboards live under `grafana/`: three JSON files (`meshmon-path`,
`meshmon-overview`, `meshmon-agent`), a datasources provisioning template,
and a Rust-based contract-drift guard.

Validate on every change:

```bash
cargo test -p meshmon-service --test grafana_contract   # JSON + panels.json contract (hermetic)
cargo e2e                                               # optional: end-to-end dashboards-provisioned smoke
```

See `grafana/README.md` for the dashboard contract, the auth posture
(meshmon proxies an internal Grafana in `auth.proxy` mode — anonymous
access is forbidden), and the editing workflow.

## Documentation

When you add or change something essential, create or update the matching `.md` — per-folder `README.md`, `CLAUDE.md` at any level where conventions are non-obvious (root or subdirectory), or feature docs under `docs/`. Skip trivia; cover what a future reader needs.

Write present tense for the current state. No change logs, "previously", progress notes, or task/PR references.

## Conventions

- Squash-merge only (no merge commits, no rebase)
- `cargo fmt` + `cargo clippy -- -D warnings` must pass
- Test coverage: unit tests in `#[cfg(test)]` modules, integration tests in `tests/`
