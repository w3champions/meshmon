-- Reverse of 20260414120000_initial.up.sql. Dropping the tables also drops
-- their indexes and foreign keys. If TimescaleDB hypertable setup has been
-- applied, dropping the base table cascades to the hypertable metadata too.
DROP TABLE IF EXISTS route_snapshots;
DROP TABLE IF EXISTS agents;
