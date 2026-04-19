-- Rollback of 20260419120000_ip_catalogue.up.sql.
--
-- The up migration moved `agents.lat`/`agents.lon` into `ip_catalogue`
-- and backfilled existing values. The down migration restores the
-- columns on `agents` and — critically — copies coordinates back from
-- `ip_catalogue` *before* dropping that table, otherwise any geo data
-- that existed at up-migration time would be lost on rollback.
--
-- WARNING: operator-only edits that have no home on `agents` (notes,
-- display_name, network_operator, website, asn, city, country, …) and
-- catalogue rows for IPs that never had a matching `agents` row are
-- still lost on rollback — by design, those columns do not exist on
-- the old schema.

DROP VIEW IF EXISTS agents_with_catalogue;

ALTER TABLE agents ADD COLUMN lat DOUBLE PRECISION;
ALTER TABLE agents ADD COLUMN lon DOUBLE PRECISION;

-- Symmetrical with the up migration's backfill: restore whichever
-- coordinate the catalogue holds, even when only one of the two is
-- present. Filtering on both-non-null would silently drop partial
-- data on rollback.
UPDATE agents a
SET lat = c.latitude,
    lon = c.longitude
FROM ip_catalogue c
WHERE c.ip = a.ip
  AND (c.latitude IS NOT NULL OR c.longitude IS NOT NULL);

DROP INDEX IF EXISTS idx_ip_catalogue_pending_sweep;
DROP TABLE IF EXISTS ip_catalogue;
DROP TYPE  IF EXISTS catalogue_source;
DROP TYPE  IF EXISTS enrichment_status;
