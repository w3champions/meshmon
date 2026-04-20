-- T45: dispatch-transport teardown. Reverses 20260421120000_campaigns_dispatch.up.sql.

ALTER TABLE agents DROP COLUMN IF EXISTS campaign_max_concurrency;

ALTER TABLE measurements DROP COLUMN IF EXISTS mtr_id;

DROP TABLE IF EXISTS mtr_traces;
