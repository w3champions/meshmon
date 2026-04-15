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
`force_refresh()` — the agent-register handler calls the latter after
writing a new row so the new agent is visible to `/api/agent/targets`
without waiting for the next tick.

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

## Configuration

See `meshmon.toml` (canonical form lives in the deploy/ example). Secrets go through `*_env` indirection.
