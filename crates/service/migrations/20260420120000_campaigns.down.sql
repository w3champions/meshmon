DROP TRIGGER  IF EXISTS measurement_campaigns_notify ON measurement_campaigns;
DROP FUNCTION IF EXISTS measurement_campaigns_notify();

DROP TABLE IF EXISTS campaign_pairs;
DROP TABLE IF EXISTS measurement_campaigns;
DROP TABLE IF EXISTS measurements;

DROP TYPE IF EXISTS measurement_kind;
DROP TYPE IF EXISTS evaluation_mode;
DROP TYPE IF EXISTS pair_resolution_state;
DROP TYPE IF EXISTS campaign_state;
DROP TYPE IF EXISTS probe_protocol;
