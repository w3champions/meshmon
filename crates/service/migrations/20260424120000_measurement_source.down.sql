-- Reverse of 20260424120000_measurement_source.up.sql.

DROP INDEX IF EXISTS measurements_vm_baseline_idx;

ALTER TABLE measurements
    DROP COLUMN IF EXISTS source;

DROP TYPE IF EXISTS measurement_source;
