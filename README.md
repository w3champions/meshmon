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

## Running tests

```bash
cargo test --workspace --all-targets
```

`meshmon-service`'s migration tests spin up a throwaway `timescale/timescaledb`
container via [`testcontainers`](https://crates.io/crates/testcontainers), so
the Docker daemon must be running locally. All tests in a single test binary
share one container; each test carves its own throwaway database inside it.

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
