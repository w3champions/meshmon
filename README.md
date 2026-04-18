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
├── grafana/                # Grafana dashboards, provisioning template, contract guard
├── docker/                 # Multi-stage Dockerfiles for service and agent
├── deploy/                 # docker-compose + example config for standalone
└── .github/workflows/      # CI
```

Subsystems:

- **Alerting:** VMAlert rules + Alertmanager Discord routing; see
  [`deploy/alerts/README.md`](deploy/alerts/README.md).

## Dashboards

Meshmon ships three Grafana dashboards under `grafana/` (per-path,
fleet-overview, per-agent) plus a datasources provisioning template for
VictoriaMetrics and Postgres. The bundled compose bakes them into
`ghcr.io/w3champions/meshmon-grafana`; operators pointing at their own
Grafana use the template. See `grafana/README.md` for the operator guide.

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

> ⚠️ **Do not expose this on the public internet without TLS.** See
> [`docs/deployment.md`](docs/deployment.md) § Enabling TLS for the two
> supported TLS paths. The block below is a localhost-only bootstrap.

```bash
# 1. Configure secrets.
cp deploy/.env.example deploy/.env
$EDITOR deploy/.env

cp deploy/meshmon.example.toml deploy/meshmon.toml

# 2. Bring up the bundled stack (service + Postgres + VM + vmalert + AM + bundled Grafana).
cd deploy && docker compose up -d           # pulls ghcr.io/w3champions/* images
# or:
cd deploy && docker compose up -d --build   # builds meshmon-service + meshmon-grafana locally

# 3. Log in at http://localhost:8080/ as admin.
```

See `deploy/meshmon.example.toml` for the full service configuration
surface and [`docs/deployment.md`](docs/deployment.md) for the
end-to-end OSS deployment guide (TLS, agent run instructions,
external-Grafana datasource wiring, vmalert-vs-AM explainer).

## Published images

Three images publish to GHCR on every push to `main` and on tagged releases,
built for `linux/amd64` and `linux/arm64`:

| Image | Pull command |
|---|---|
| Service (API + embedded SPA) | `docker pull ghcr.io/w3champions/meshmon-service:latest` |
| Agent (probe worker) | `docker pull ghcr.io/w3champions/meshmon-agent:latest` |
| Grafana (bundled dashboards + provisioning) | `docker pull ghcr.io/w3champions/meshmon-grafana:latest` |

Available tags:
- `:latest` — head of `main` after a successful publish + Trivy scan.
- `:main-<sha>` — the immutable per-commit tag. Pin to this in production.
- `:v<major>.<minor>.<patch>` — emitted when a `v*` git tag pushes.

`deploy/docker-compose.yml` references `:latest` by default. For reproducible
deploys, override each service's `image:` to `:main-<sha>` (or `:v<ver>`).

## Running CI checks locally

Every CI job has a one-liner local equivalent that reads the same config
file CI does — no duplication to maintain.

| CI job | Local command | Shared config |
|---|---|---|
| Rust fmt | `cargo fmt --all -- --check` | `rust-toolchain.toml` |
| Rust clippy | `cargo clippy --workspace --all-targets -- -D warnings` | `Cargo.toml` |
| Rust tests | `cargo xtask test` | `Cargo.toml` |
| OpenAPI snapshot drift (Rust) | `cargo xtask openapi && git diff --exit-code frontend/src/api/openapi.gen.json` | — |
| OpenAPI types drift (frontend) | `cd frontend && npm run openapi:types && git diff --exit-code src/api/schema.gen.ts` | `frontend/package.json` |
| Frontend lint | `cd frontend && npx biome check ./src` | `frontend/biome.json` |
| Frontend type-check | `cd frontend && npm run type-check` | `frontend/tsconfig.json` |
| Frontend tests | `cd frontend && npm test` | `frontend/vitest.config.ts` |
| Frontend build | `cd frontend && npm run build` | `frontend/package.json` |
| yamllint | `yamllint -c .yamllint.yml deploy` | `.yamllint.yml` |
| shellcheck | `shellcheck --severity=warning scripts/*.sh` | — |
| actionlint | `actionlint .github/workflows/*.yml` | — |
| Rust E2E | `cd deploy && docker compose up -d --build --wait && cd .. && cargo e2e && cd deploy && docker compose down -v` | `deploy/docker-compose.yml` |
| Release binary (frontend-embedded) | `cd frontend && npm ci && npm run build && cd .. && cargo build --release -p meshmon-service` | `crates/service/build.rs` |

> The `Release binary` CI job additionally uploads the compiled binary as a GitHub Actions artifact; that step has no local equivalent.

Install the non-cargo tools with your package manager (macOS:
`brew install yamllint shellcheck actionlint`). See **Pre-commit hooks
(optional)** below for a lefthook-wrapped workflow that runs the fast
subset on every commit.

## Pre-commit hooks (optional)

This repo ships a `lefthook.yml` that runs the same formatters and linters
used in CI, scoped to staged files. To opt in:

```bash
# Install lefthook once:
#   macOS:  brew install lefthook
#   Linux:  curl -1sLf 'https://raw.githubusercontent.com/evilmartians/lefthook/master/install.sh' | sh
#   Other:  see https://lefthook.dev/installation/

# Inside the cloned repo:
lefthook install
```

Requirements when the hooks run: `cargo`, `npx` (via frontend deps),
`yamllint`, `shellcheck`, `actionlint`. Missing a tool skips that step
locally — CI still enforces all of them.

Skip a hook for a single commit with `LEFTHOOK=0 git commit` (entire
hook bypass) or `git commit --no-verify` (standard git flag). Do not
merge bypassed commits — the CI failure will surface anyway.

For local frontend + backend iteration with HMR:

```bash
./scripts/dev.sh
```

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

### One-command dev loop

`scripts/dev.sh` brings up the bundled infra (Postgres + VictoriaMetrics
+ Grafana + Alertmanager + vmalert) via the dev compose overlay, seeds a
couple of agents and route snapshots, starts `cargo run -p meshmon-service`
in the background, and runs the Vite dev server in the foreground
(HMR, `/api` proxied to the service). Ctrl-C tears everything down.

```bash
./scripts/dev.sh
# Open http://127.0.0.1:5173/  —  login: admin / smoketest
```

Intended for local UI iteration only — for the full production stack
(including the service container from `docker/Dockerfile.service`), see
the Quick start (Docker) block above and `docs/deployment.md`.

Requires `docker`, `cargo`, `argon2`, `openssl`, `psql`, `sqlx`, and `npm`
on `$PATH`.

Signals:
- `SIGINT`, `SIGTERM` — graceful shutdown.
- `SIGHUP` — re-read `meshmon.toml` (hot-reload for `probing`, `logging`, `upstream`; restart required for `service.listen_addr`, `auth`, `database`).

## Bundled Grafana + Alertmanager

meshmon-service is the only public HTTP face in the bundled OSS
deployment. Grafana and Alertmanager sit on the internal docker
bridge with no host port mapping; operators reach them via
meshmon-service's authenticated reverse proxies:

- `/grafana/*` → bundled Grafana in `auth.proxy` mode. The proxy
  injects `X-WEBAUTH-USER` from the operator's meshmon session,
  so there is no second login. Configure the upstream via
  `[upstream] grafana_url` in `meshmon.toml` (or
  `grafana_url_env`). The SPA hardcodes the `/grafana` prefix — no
  client-side Grafana URL configuration.
- `/alertmanager/*` → bundled Alertmanager. No auth-proxy header;
  AM has no `auth.proxy` equivalent and relies on bridge isolation
  + meshmon's edge session check.
- `meshmon_grafana` Postgres role — created `NOLOGIN` by the
  service's migrations. Set `MESHMON_PG_GRAFANA_PASSWORD` before
  starting the service and it flips the role to `LOGIN` with that
  password atomically. Without the env var, the role stays
  `NOLOGIN` and the bundled Grafana's `MeshmonPostgres` datasource
  fails with "role is not permitted to log in" — which is the
  correct failure mode (loud, unambiguous).

## Regenerating the OpenAPI snapshot

The frontend consumes `frontend/src/api/openapi.json` for TypeScript type generation. Whenever a handler's `#[utoipa::path]` annotation changes, or a new handler lands, run:

```bash
cargo xtask openapi
```

CI re-runs this and fails if the checked-in snapshot is stale.

## Running tests

```sh
cargo xtask test            # primary path (provisions TimescaleDB, runs nextest)
cargo xtask test-e2e        # compose-stack end-to-end tests
cargo xtask test-db down    # tear down the shared test database
```

`cargo xtask test` provisions a single shared `timescale/timescaledb`
container, sets `DATABASE_URL`, and runs `cargo nextest` against it. This
is the canonical path for local dev and CI.

`cargo test --workspace --all-targets` still works as a zero-setup
fallback — it spawns one container per test binary via
[`testcontainers`](https://crates.io/crates/testcontainers) and tears
each down at process exit. `cargo nextest run` without `DATABASE_URL`
is not supported and will panic with a clear message directing you to
`cargo xtask test`.

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
