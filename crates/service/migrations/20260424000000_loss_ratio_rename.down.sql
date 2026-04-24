-- Revert the `loss_ratio` rename: multiply threshold columns back to
-- percent, rename columns to their `_pct` names, restore the default
-- of 2.0 on `measurement_campaigns.loss_threshold_pct`.

UPDATE campaign_evaluations
    SET loss_threshold_ratio = loss_threshold_ratio * 100.0;

ALTER TABLE campaign_evaluations
    RENAME COLUMN loss_threshold_ratio TO loss_threshold_pct;

ALTER TABLE measurement_campaigns
    ALTER COLUMN loss_threshold_ratio SET DEFAULT 2.0;

UPDATE measurement_campaigns
    SET loss_threshold_ratio = loss_threshold_ratio * 100.0;

ALTER TABLE measurement_campaigns
    RENAME COLUMN loss_threshold_ratio TO loss_threshold_pct;

ALTER TABLE measurements
    RENAME COLUMN loss_ratio TO loss_pct;
