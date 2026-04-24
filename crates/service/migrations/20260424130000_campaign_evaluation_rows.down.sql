BEGIN;

DROP TABLE campaign_evaluation_unqualified_reasons;
DROP TABLE campaign_evaluation_pair_details;
DROP TABLE campaign_evaluation_candidates;
DROP TYPE pair_detail_direct_source;

ALTER TABLE campaign_evaluations ADD COLUMN results JSONB;
-- NOT NULL was the original; but there are no rows to fill, so leave nullable.
-- Operators roll-forward, so the down path is for dev only.

DROP INDEX campaign_evaluations_campaign_evaluated_idx;
ALTER TABLE campaign_evaluations ADD CONSTRAINT campaign_evaluations_campaign_id_key UNIQUE (campaign_id);

COMMIT;
