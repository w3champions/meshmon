BEGIN;

DROP TABLE IF EXISTS campaign_evaluation_qualifying_legs;

ALTER TABLE campaign_evaluation_candidates
    DROP COLUMN IF EXISTS avg_loss_ratio;

COMMIT;
