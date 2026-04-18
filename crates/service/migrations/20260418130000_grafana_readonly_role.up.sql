-- Read-only Postgres role consumed by the bundled Grafana's
-- `MeshmonPostgres` datasource.
--
-- SECURITY: created `NOLOGIN` deliberately. A `LOGIN` role with no
-- password would be authenticatable under `pg_hba.conf` entries that
-- use `trust` (accidents happen, especially on dev hosts); `NOLOGIN`
-- is unauthenticatable regardless of `pg_hba.conf`. The service flips
-- `NOLOGIN` → `LOGIN PASSWORD '...'` in a single `ALTER ROLE` from
-- `MESHMON_PG_GRAFANA_PASSWORD` after migrations — see
-- `crates/service/src/db.rs::apply_grafana_role_password`.
--
-- Running this migration twice is a no-op: the exception-swallowed
-- CREATE ROLE, REVOKEs, GRANTs, and ALTER DEFAULT PRIVILEGES are all
-- idempotent.
--
-- CONCURRENCY: roles are cluster-global, so two sessions migrating
-- different databases on the same cluster race on `pg_catalog` rows
-- (manifesting as `duplicate_object` or `XX000: tuple concurrently
-- updated`). A cluster-wide advisory lock serializes the role-touching
-- DDL in this migration. The key (4_851_623_871) is an arbitrary
-- 32-bit constant unique to this migration. The lock is held for the
-- remainder of the session/transaction and released automatically.

SELECT pg_advisory_xact_lock(4851623871);

-- `CREATE ROLE ... IF NOT EXISTS` isn't a thing in Postgres. The
-- advisory lock above guarantees only one session reaches this block
-- at a time on a given cluster, but swallowing `duplicate_object` is
-- still defensive — it keeps the migration safe if the lock ever gets
-- bypassed by a future operator running this SQL manually.
DO $$
BEGIN
    CREATE ROLE meshmon_grafana NOLOGIN;
EXCEPTION
    WHEN duplicate_object OR unique_violation THEN
        -- `duplicate_object` fires when the role already existed before
        -- this session started; `unique_violation` fires when a
        -- concurrent session raced us and committed first. Both mean
        -- "the role is there now" — exactly the desired end state.
        NULL;
END$$;

-- Explicit REVOKE ALL first so a role that was accidentally granted
-- wider privileges is narrowed back down on every run.
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM meshmon_grafana;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM meshmon_grafana;

-- Grant SELECT only on the tables the bundled dashboards query.
GRANT USAGE ON SCHEMA public TO meshmon_grafana;
GRANT SELECT ON agents TO meshmon_grafana;
GRANT SELECT ON route_snapshots TO meshmon_grafana;

-- Explicitly DENY access to future sensitive tables by setting the
-- default privileges for new tables to nothing for this role.
ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE ALL ON TABLES FROM meshmon_grafana;
