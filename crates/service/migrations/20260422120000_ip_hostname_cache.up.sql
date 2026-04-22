-- IP→hostname reverse-DNS cache. Append-only table promoted to a
-- TimescaleDB hypertable (7-day chunks) with a 90-day retention policy
-- in `db::apply_timescaledb_setup`, mirroring how `route_snapshots` is
-- handled: plain DDL stays in the migration so it applies cleanly on
-- plain Postgres, and hypertable / retention DDL is installed out-of-
-- band only when the `timescaledb` extension is present.
--
-- hostname IS NULL marks a confirmed NXDOMAIN (negative cache).
-- Transient resolver failures are never written.

CREATE TABLE ip_hostname_cache (
    ip          INET        NOT NULL,
    hostname    TEXT        NULL,
    resolved_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX ip_hostname_cache_ip_latest
    ON ip_hostname_cache (ip, resolved_at DESC);
