-- Initial meshmon schema.
--
-- Tables created here contain no TimescaleDB-specific DDL so that this
-- migration applies uniformly on plain Postgres and on TimescaleDB-enabled
-- Postgres. Hypertable, compression, and retention setup runs from the Rust
-- side in meshmon_service::db::run_migrations after the base migration has
-- been applied.

-- === agents ================================================================
-- Who has registered, when, with what metadata.
-- Written on /api/agent/register; last_seen_at touched on every metrics push.
CREATE TABLE agents (
    id              TEXT PRIMARY KEY,
    display_name    TEXT NOT NULL,
    location        TEXT,
    ip              INET NOT NULL,
    lat             DOUBLE PRECISION,
    lon             DOUBLE PRECISION,
    agent_version   TEXT,
    registered_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_agents_last_seen ON agents (last_seen_at DESC);

-- === route_snapshots =======================================================
-- One row per "meaningful route change" detected by an agent.
-- Append-only. JSONB hops keep snapshot self-contained and queryable.
-- Primary key includes observed_at so that TimescaleDB can later partition by
-- time without having to rewrite the key.
CREATE TABLE route_snapshots (
    id              BIGSERIAL,
    source_id       TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
    target_id       TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
    protocol        TEXT NOT NULL CHECK (protocol IN ('icmp','tcp','udp')),
    observed_at     TIMESTAMPTZ NOT NULL,
    hops            JSONB NOT NULL,
    path_summary    JSONB,
    PRIMARY KEY (id, observed_at)
);

CREATE INDEX idx_route_snapshots_lookup
    ON route_snapshots (source_id, target_id, observed_at DESC);

CREATE INDEX idx_route_snapshots_hops_gin
    ON route_snapshots USING GIN (hops);
