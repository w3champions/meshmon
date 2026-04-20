DROP TABLE IF EXISTS campaign_evaluations;
DROP INDEX IF EXISTS campaign_pairs_campaign_kind_idx;
ALTER TABLE campaign_pairs DROP COLUMN IF EXISTS kind;
