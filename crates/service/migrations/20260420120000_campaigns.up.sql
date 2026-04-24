-- T44: campaign data model + scheduler.
--
-- Introduces the measurement-campaign pair table, its parent
-- `measurement_campaigns` catalog, a minimal `measurements` table used by
-- the 24 h reuse lookup and size-preview query, and a NOTIFY trigger
-- (`campaign_state_changed`) so the in-process scheduler wakes on
-- lifecycle transitions.
--
-- `measurements` here carries only the columns spec 02 §2.3 references.
-- A follow-up dispatch-transport migration adds `mtr_traces`,
-- `measurements.mtr_id`, and the FK `campaign_pairs.measurement_id →
-- measurements(id)` once the dispatch writer exists.

-- ENUMs ------------------------------------------------------------------
CREATE TYPE probe_protocol        AS ENUM ('icmp', 'tcp', 'udp');
CREATE TYPE campaign_state        AS ENUM ('draft', 'running', 'completed', 'evaluated', 'stopped');
CREATE TYPE pair_resolution_state AS ENUM ('pending', 'dispatched', 'reused', 'succeeded', 'unreachable', 'skipped');
CREATE TYPE evaluation_mode       AS ENUM ('diversity', 'optimization');
CREATE TYPE measurement_kind      AS ENUM ('campaign', 'detail_ping', 'detail_mtr');

-- Measurements -----------------------------------------------------------
-- Minimal skeleton used by the 24 h reuse lookup. A follow-up migration
-- extends this table with `mtr_id` and creates the sibling `mtr_traces`
-- table.
CREATE TABLE measurements (
    id                  BIGSERIAL PRIMARY KEY,
    source_agent_id     TEXT NOT NULL,
    destination_ip      INET NOT NULL,
    protocol            probe_protocol NOT NULL,
    probe_count         SMALLINT NOT NULL,
    measured_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    latency_min_ms      REAL,
    latency_avg_ms      REAL,
    latency_median_ms   REAL,
    latency_p95_ms      REAL,
    latency_max_ms      REAL,
    latency_stddev_ms   REAL,
    loss_pct            REAL NOT NULL DEFAULT 0.0,
    kind                measurement_kind NOT NULL DEFAULT 'campaign'
);

CREATE INDEX measurements_reuse_idx
  ON measurements (source_agent_id, destination_ip, protocol, probe_count DESC, measured_at DESC);

-- Measurement campaigns --------------------------------------------------
CREATE TABLE measurement_campaigns (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title                   TEXT NOT NULL,
    notes                   TEXT NOT NULL DEFAULT '',
    state                   campaign_state NOT NULL DEFAULT 'draft',
    protocol                probe_protocol NOT NULL,
    probe_count             SMALLINT NOT NULL DEFAULT 10,
    probe_count_detail      SMALLINT NOT NULL DEFAULT 250,
    timeout_ms              INTEGER NOT NULL DEFAULT 2000,
    probe_stagger_ms        INTEGER NOT NULL DEFAULT 100,
    force_measurement       BOOLEAN NOT NULL DEFAULT FALSE,
    loss_threshold_pct      REAL NOT NULL DEFAULT 2.0,
    stddev_weight           REAL NOT NULL DEFAULT 1.0,
    evaluation_mode         evaluation_mode NOT NULL DEFAULT 'optimization',
    created_by              TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at              TIMESTAMPTZ,
    stopped_at              TIMESTAMPTZ,
    completed_at            TIMESTAMPTZ,
    evaluated_at            TIMESTAMPTZ
);

-- Indexes per spec 02 §2.1.
CREATE INDEX measurement_campaigns_search_idx
  ON measurement_campaigns USING GIN (to_tsvector('simple', title || ' ' || notes));
CREATE INDEX measurement_campaigns_state_started_idx
  ON measurement_campaigns (state, started_at);
CREATE INDEX measurement_campaigns_created_by_idx
  ON measurement_campaigns (created_by);

-- Campaign pairs ---------------------------------------------------------
CREATE TABLE campaign_pairs (
    id                BIGSERIAL PRIMARY KEY,
    campaign_id       UUID NOT NULL REFERENCES measurement_campaigns(id) ON DELETE CASCADE,
    source_agent_id   TEXT NOT NULL,
    destination_ip    INET NOT NULL,
    resolution_state  pair_resolution_state NOT NULL DEFAULT 'pending',
    measurement_id    BIGINT REFERENCES measurements(id),
    dispatched_at     TIMESTAMPTZ,
    settled_at        TIMESTAMPTZ,
    attempt_count     SMALLINT NOT NULL DEFAULT 0,
    last_error        TEXT,
    UNIQUE (campaign_id, source_agent_id, destination_ip)
);

CREATE INDEX campaign_pairs_state_idx
  ON campaign_pairs (campaign_id, resolution_state);
CREATE INDEX campaign_pairs_settled_idx
  ON campaign_pairs (campaign_id, settled_at DESC);

-- NOTIFY trigger ---------------------------------------------------------
-- Fires on any state change so the scheduler's LISTEN loop wakes ahead
-- of the 500 ms tick. Payload is the campaign UUID; well under pg_notify's
-- 8000-byte cap.
CREATE OR REPLACE FUNCTION measurement_campaigns_notify() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('campaign_state_changed', NEW.id::text);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER measurement_campaigns_notify
AFTER INSERT OR UPDATE OF state ON measurement_campaigns
FOR EACH ROW
EXECUTE FUNCTION measurement_campaigns_notify();
