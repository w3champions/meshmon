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
--
-- PRIVILEGE FALLBACK: managed Postgres offerings (AWS RDS, GCP Cloud
-- SQL, Supabase, Neon, ...) often give the application user DDL on
-- its own database but NOT cluster-level `CREATEROLE`. Running this
-- migration under such a user would fail startup with
-- `insufficient_privilege`, which is worse than doing nothing (the
-- bundled Grafana datasource is an optional feature). Catch that
-- specific error and `RAISE WARNING` instead — the operator who
-- wants the bundled Grafana wires up the role manually, and
-- `apply_grafana_role_password` skips its ALTER when the role is
-- absent.

SELECT pg_advisory_xact_lock(4851623871);

DO $$
BEGIN
    -- Create role idempotently. `duplicate_object` fires when the
    -- role pre-exists; `unique_violation` fires when a concurrent
    -- session committed first. `insufficient_privilege` means the
    -- current user lacks `CREATEROLE` — bail out of the whole
    -- provisioning so the migration still succeeds.
    BEGIN
        CREATE ROLE meshmon_grafana NOLOGIN;
    EXCEPTION
        WHEN duplicate_object OR unique_violation THEN
            NULL;
        WHEN insufficient_privilege THEN
            RAISE WARNING 'skipping meshmon_grafana provisioning: current user lacks CREATEROLE. '
                          'The bundled Grafana datasource will not work until the role is '
                          'created manually. See grafana/README.md.';
            RETURN;
    END;

    -- Role exists now. Tighten privileges to SELECT-only on the
    -- dashboard tables.
    REVOKE ALL ON ALL TABLES IN SCHEMA public FROM meshmon_grafana;
    REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM meshmon_grafana;
    GRANT USAGE ON SCHEMA public TO meshmon_grafana;
    GRANT SELECT ON agents TO meshmon_grafana;
    GRANT SELECT ON route_snapshots TO meshmon_grafana;
    ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE ALL ON TABLES FROM meshmon_grafana;
EXCEPTION
    WHEN insufficient_privilege THEN
        RAISE WARNING 'meshmon_grafana grants skipped: current user lacks the GRANT '
                      'privilege on one of the dashboard tables. Run the grants '
                      'manually if the bundled Grafana datasource is needed.';
END$$;
