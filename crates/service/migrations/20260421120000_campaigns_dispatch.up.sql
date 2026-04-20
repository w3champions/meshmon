-- T45: dispatch-transport additions.
--
-- Builds on T44's campaign schema (20260420120000_campaigns.up.sql):
--   * Creates mtr_traces, the sibling table for MTR detail runs.
--   * Extends measurements with an optional mtr_id FK.
--   * Extends agents with a campaign_max_concurrency override the
--     RpcDispatcher reads at dispatch time.

CREATE TABLE mtr_traces (
    id          BIGSERIAL PRIMARY KEY,
    captured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    hops        JSONB NOT NULL
);

ALTER TABLE measurements
    ADD COLUMN mtr_id BIGINT NULL REFERENCES mtr_traces(id);

ALTER TABLE agents
    ADD COLUMN campaign_max_concurrency SMALLINT NULL
        CHECK (campaign_max_concurrency IS NULL OR campaign_max_concurrency >= 1);
