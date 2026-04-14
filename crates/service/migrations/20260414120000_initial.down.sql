-- Reverse of 20260414120000_initial.up.sql. Dropping the tables also drops
-- their indexes and foreign keys. If TimescaleDB hypertable setup has been
-- applied, dropping the base table cascades to the hypertable metadata too.
--
-- Intentionally does NOT DROP EXTENSION timescaledb: the extension is
-- database-wide and may be relied on by other objects outside this
-- migration's scope. Operators who want it gone can run DROP EXTENSION
-- manually.
DROP TABLE IF EXISTS route_snapshots;
DROP TABLE IF EXISTS agents;
