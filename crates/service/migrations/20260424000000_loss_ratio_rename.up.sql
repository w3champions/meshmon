-- Rename `loss_pct` → `loss_ratio` columns and convert
-- `loss_threshold_*` values from percent (0–100) to ratio (0.0–1.0).
--
-- The `measurements.loss_pct` column already stored fractions on main,
-- so only the column is renamed; values are untouched. The two
-- `loss_threshold_*` columns stored percent on main and must be
-- divided by 100 to convert to ratio. Branch-era data is ephemeral
-- and out of scope.

ALTER TABLE measurements
    RENAME COLUMN loss_pct TO loss_ratio;

ALTER TABLE measurement_campaigns
    RENAME COLUMN loss_threshold_pct TO loss_threshold_ratio;

UPDATE measurement_campaigns
    SET loss_threshold_ratio = loss_threshold_ratio / 100.0;

ALTER TABLE measurement_campaigns
    ALTER COLUMN loss_threshold_ratio SET DEFAULT 0.02;

ALTER TABLE campaign_evaluations
    RENAME COLUMN loss_threshold_pct TO loss_threshold_ratio;

UPDATE campaign_evaluations
    SET loss_threshold_ratio = loss_threshold_ratio / 100.0;
