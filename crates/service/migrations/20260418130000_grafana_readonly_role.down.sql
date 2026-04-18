-- Revoke grants and drop the role if it has no remaining dependent
-- objects.

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'meshmon_grafana') THEN
        REVOKE ALL ON ALL TABLES IN SCHEMA public FROM meshmon_grafana;
        REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM meshmon_grafana;
        REVOKE ALL ON SCHEMA public FROM meshmon_grafana;
        DROP ROLE meshmon_grafana;
    END IF;
END$$;
