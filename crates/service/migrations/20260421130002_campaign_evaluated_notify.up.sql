-- Cross-instance fan-out for `/evaluate`. The in-process broker only
-- reaches clients connected to the instance that ran the evaluate
-- handler; without a NOTIFY channel, subscribers on other instances
-- never see the `evaluated` SSE frame and keep stale `/evaluation`
-- cache contents. Mirrors the pattern used by
-- `measurement_campaigns_notify` (`campaign_state_changed`) and the
-- writer's `campaign_pair_settled` channel.

CREATE OR REPLACE FUNCTION campaign_evaluations_notify() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('campaign_evaluated', NEW.campaign_id::text);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER campaign_evaluations_notify
AFTER INSERT OR UPDATE ON campaign_evaluations
FOR EACH ROW
EXECUTE FUNCTION campaign_evaluations_notify();
