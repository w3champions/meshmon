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

## Status

This repo is under active initial construction. The scaffolding in this commit
builds and runs, but the service and agent binaries are placeholders. Feature
work lands incrementally per the task plan tracked outside this repo.

## License

AGPL-3.0. See `LICENSE`.
