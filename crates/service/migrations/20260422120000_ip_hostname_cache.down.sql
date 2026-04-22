-- The hypertable + retention policy live in `apply_timescaledb_setup`
-- and are dropped by cascading from the base table. Dropping the index
-- and table is sufficient on both plain Postgres and TimescaleDB.
DROP INDEX IF EXISTS ip_hostname_cache_ip_latest;
DROP TABLE IF EXISTS ip_hostname_cache;
