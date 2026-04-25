BEGIN;

ALTER TABLE campaign_evaluations
    DROP COLUMN min_improvement_ratio,
    DROP COLUMN min_improvement_ms,
    DROP COLUMN max_transit_stddev_ms,
    DROP COLUMN max_transit_rtt_ms;

ALTER TABLE measurement_campaigns
    DROP COLUMN min_improvement_ratio,
    DROP COLUMN min_improvement_ms,
    DROP COLUMN max_transit_stddev_ms,
    DROP COLUMN max_transit_rtt_ms;

COMMIT;
