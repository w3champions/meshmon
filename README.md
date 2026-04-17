# meshmon

Continuous monitoring and alerting for the network paths between nodes in a
fully-interconnected mesh. Agents probe every other node with TCP, UDP, and
ICMP/MTR; a central service ingests, stores, and alerts on regressions and
route changes.

meshmon is self-contained and open-source. No hostnames, credentials, or node
lists specific to any deployment live in this repo.

## Repository layout

```
meshmon/
├── Cargo.toml              # Rust workspace
├── crates/
│   ├── service/            # Central axum service + API + embedded frontend
│   ├── agent/              # Per-node probe agent
│   ├── protocol/           # Shared Protobuf messages
│   ├── revtunnel/          # Reverse-tunnel transport (yamux over a tonic bidi stream)
│   └── common/             # Shared utilities
├── frontend/               # React 19 + Tailwind SPA, embedded into service
├── docker/                 # Multi-stage Dockerfiles for service and agent
├── deploy/                 # docker-compose + example config for standalone
└── .github/workflows/      # CI
```

## Quick start (development)

Prerequisites: Rust 1.94+, Node 20+, Docker.

```bash
# Build everything locally
cargo build --workspace
(cd frontend && npm install && npm run build)

# Run the service binary
cargo run --bin meshmon-service

# Run the agent binary
cargo run --bin meshmon-agent

# Frontend dev server (with hot reload + API proxy to localhost:8080)
(cd frontend && npm run dev)
```

## Quick start (Docker)

```bash
# Build the images
docker build -f docker/Dockerfile.service -t meshmon-service:dev .
docker build -f docker/Dockerfile.agent   -t meshmon-agent:dev   .

# Standalone stack
cp deploy/.env.example deploy/.env
$EDITOR deploy/.env
docker compose -f deploy/docker-compose.yml up -d
```

See `deploy/meshmon.example.toml` for the service configuration surface.

## Running the service

```bash
# 1. Provision Postgres + TimescaleDB (for local dev: a single docker run).
docker run --rm -d --name meshmon-db \
    -e POSTGRES_PASSWORD=meshmon \
    -e POSTGRES_USER=meshmon \
    -e POSTGRES_DB=meshmon \
    -p 5432:5432 \
    timescale/timescaledb:2.26.3-pg16

# 2. Copy the example config.
cp deploy/meshmon.example.toml /tmp/meshmon.toml
# Edit /tmp/meshmon.toml: set `url = "postgres://meshmon:meshmon@localhost:5432/meshmon"` or similar.

# 3. Run.
MESHMON_CONFIG=/tmp/meshmon.toml cargo run --package meshmon-service
```

The service binds on `service.listen_addr` (default `0.0.0.0:8080`). Useful endpoints while it's running:
- `GET /healthz` — always 200 if the process is up.
- `GET /readyz` — 200 after migrations apply; 503 during shutdown.
- `GET /metrics` — Prometheus text format (`meshmon_service_*`).
- `GET /api/docs` — Swagger UI for the operator API.
- `GET /api/openapi.json` — OpenAPI 3.1 schema (also checked in at `frontend/src/api/openapi.json`).

### One-command smoke harness

`scripts/smoke.sh` brings up Postgres + VictoriaMetrics in Docker, writes a
throwaway config, seeds a few agents and route snapshots, starts the service
in the background, and runs the Vite dev server in the foreground (which
proxies `/api` to the service). Ctrl-C tears everything down.

```bash
./scripts/smoke.sh
# Open http://127.0.0.1:5173/  —  login: admin / smoketest
```

Intended for local UI smoke-testing only — see `deploy/docker-compose.yml`
for the full production stack.

Requires `docker`, `cargo`, `argon2`, `openssl`, `psql`, `sqlx`, and `npm`
on `$PATH`.

Signals:
- `SIGINT`, `SIGTERM` — graceful shutdown.
- `SIGHUP` — re-read `meshmon.toml` (hot-reload for `probing`, `logging`, `upstream`; restart required for `service.listen_addr`, `auth`, `database`).

## Regenerating the OpenAPI snapshot

The frontend consumes `frontend/src/api/openapi.json` for TypeScript type generation. Whenever a handler's `#[utoipa::path]` annotation changes, or a new handler lands, run:

```bash
cargo xtask openapi
```

CI re-runs this and fails if the checked-in snapshot is stale.

## Running tests

```bash
cargo test --workspace --all-targets
```

`meshmon-service`'s integration tests spin up one
`timescale/timescaledb` container per test binary via
[`testcontainers`](https://crates.io/crates/testcontainers) and share it
across every test in that binary, so the Docker daemon must be running
locally. A process-exit hook removes the container; if you kill a test
run with Ctrl-C the container may survive — prune with
`docker ps -a --filter ancestor=timescale/timescaledb | awk 'NR>1 {print $1}' | xargs -r docker rm -f`.

To target an existing Postgres (e.g. a remote instance) instead of
auto-spawning, set `DATABASE_URL`:

```bash
export DATABASE_URL="postgres://user:pass@host:port/postgres"
cargo test -p meshmon-service --test migrations
```

### Database queries (sqlx offline cache)

The service uses `sqlx::query!` / `sqlx::query_as!` macros for compile-time-checked SQL. The macros validate against either a live `DATABASE_URL` or the committed `.sqlx/` offline cache.

```bash
# One-time:
cargo install sqlx-cli --no-default-features --features rustls,postgres --version ~0.8

# After changing any query!/query_as! macro: bring up Postgres, regenerate, commit.
docker run -d --rm --name meshmon-prep -p 55432:5432 \
    -e POSTGRES_PASSWORD=meshmon timescale/timescaledb:2.26.3-pg16
sleep 3
export DATABASE_URL=postgres://postgres:meshmon@127.0.0.1:55432/postgres
sqlx migrate run --source crates/service/migrations
cargo sqlx prepare --workspace -- --all-targets --all-features
git add .sqlx
docker stop meshmon-prep
```

CI sets `SQLX_OFFLINE=true` and runs `cargo sqlx prepare --check` to guard against stale caches.

## Running the agent

The agent binary is deployed to each monitored node. It registers with the
central service, fetches probe configuration and the target list, then spawns
one supervisor task per target.

### Environment variables

| Variable | Required | Description |
|----------|----------|-------------|
| `MESHMON_SERVICE_URL` | yes | URL of the central service (`http://` or `https://`) |
| `MESHMON_AGENT_TOKEN` | yes | Shared bearer token for gRPC authentication |
| `AGENT_ID` | yes | Unique machine-readable name, e.g. `brazil-north` |
| `AGENT_DISPLAY_NAME` | yes | Human-friendly label |
| `AGENT_LOCATION` | yes | Free-form location string |
| `AGENT_IP` | yes | Externally-reachable IP (v4 or v6) |
| `AGENT_LAT` | yes | Latitude in decimal degrees (-90 to 90) |
| `AGENT_LON` | yes | Longitude in decimal degrees (-180 to 180) |
| `MESHMON_TCP_PROBE_PORT` | yes | Port the TCP echo listener binds (dual-stack on `[::]`, serves both IPv4 and IPv6 peers). Must be open on the host and reachable from peers. |
| `MESHMON_UDP_PROBE_PORT` | yes | Port the UDP echo listener binds (dual-stack on `[::]`, serves both IPv4 and IPv6 peers). Must be open on the host and reachable from peers. |
| `MESHMON_ICMP_TARGET_CONCURRENCY` | no (default `32`) | Global cap on concurrent per-target ICMP/traceroute rounds. Lower if raw-socket / thread use is too high. |
| `RUST_LOG` | no | Tracing filter (default: `meshmon_agent=info,warn`) |

The agent needs raw-socket access for ICMP/traceroute probes. In Docker,
grant the container the `NET_RAW` and `NET_ADMIN` capabilities:

```yaml
services:
  meshmon-agent:
    image: ghcr.io/w3champions/meshmon-agent:latest
    cap_add:
      - NET_RAW
      - NET_ADMIN
    ports:
      - "3555:3555/tcp"   # MESHMON_TCP_PROBE_PORT
      - "3552:3552/udp"   # MESHMON_UDP_PROBE_PORT
```

On bare metal, either run as root or set `CAP_NET_RAW,CAP_NET_ADMIN` on
the binary with `setcap`.

```bash
export MESHMON_SERVICE_URL=http://localhost:8080
export MESHMON_AGENT_TOKEN=<token>
export AGENT_ID=dev-agent
export AGENT_DISPLAY_NAME="Dev Agent"
export AGENT_LOCATION="Local"
export AGENT_IP=127.0.0.1
export AGENT_LAT=0.0
export AGENT_LON=0.0
export MESHMON_TCP_PROBE_PORT=3555
export MESHMON_UDP_PROBE_PORT=3552

cargo run -p meshmon-agent
```

### Agent modules

| Module | Responsibility |
|--------|----------------|
| `config.rs` | Env var parsing (`AgentEnv`), probe config wrapper |
| `api.rs` | `ServiceApi` trait + `GrpcServiceApi` — tonic client over a cloneable HTTP/2 `Channel` (no mutex; concurrent RPCs multiplex over one connection) |
| `stats.rs` | Per-protocol rolling stats + on-demand `Summary` with percentiles |
| `state.rs` | Pure state-machine types: per-protocol + path health, rate/window lookup |
| `route.rs` | Per-target route-state tracker: accumulates trippy per-hop observations over a rolling window, builds canonical snapshots, detects meaningful diffs |
| `supervisor.rs` | Per-target supervisor: spawns 4 probers, runs state machine every 10 s, publishes rates, emits diff-gated route snapshots on a 60 s tick, and pushes one `PathMetricsMsg` per healthy protocol on an independent 60 s metrics tick |
| `emitter.rs` | Single outbound task: batches `PathMetricsMsg` into `MetricsBatch` every 60 s, pushes route snapshots immediately, retries retriable failures (UNAVAILABLE / RESOURCE_EXHAUSTED / transport errors) with jittered 1 s → 5 min backoff, buffers up to 65 failed RPCs in a drop-oldest ring queue with `dropped_count` reporting |
| `bootstrap.rs` | Register → config → targets → spawn emitter + per-target supervisors, 5-minute refresh loop |
| `probing/mod.rs` | `ProbeObservation` / `HopObservation` types (populated by probers) |
| `probing/icmp.rs` | ICMP Echo pinger (`surge-ping`), always-on per target for per-protocol health |

### Lifecycle

1. Parse identity from env vars (fail fast on missing/invalid values).
2. Connect to the service via gRPC with bearer-token interceptor.
3. Register with exponential-backoff retry (1 s → 30 s, ±25% jitter).
4. Fetch initial config and target list.
5. Spawn the emitter task (consumes metrics + route-snapshot channels).
6. Spawn one supervisor task per target (skip self); each clones the two channel senders.
7. Run a 5-minute refresh loop: re-fetch config (broadcast via `watch`), re-fetch targets (reconcile: spawn new, shut down removed).
8. On `SIGTERM` / `SIGINT`: cancel all supervisors, drop channel senders, await the emitter's 5 s shutdown drain, exit.

## Status

This repo is under active initial construction. Feature work lands
incrementally.

## License

AGPL-3.0. See `LICENSE`.
