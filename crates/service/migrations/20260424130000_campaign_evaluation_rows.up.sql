BEGIN;

-- Discard legacy rows (approved — no data-migration needed)
TRUNCATE campaign_evaluations;

-- Drop one-per-campaign constraint so re-evaluate creates history
ALTER TABLE campaign_evaluations DROP CONSTRAINT campaign_evaluations_campaign_id_key;

-- Add an index tuned for "latest evaluation per campaign"
CREATE INDEX campaign_evaluations_campaign_evaluated_idx
    ON campaign_evaluations (campaign_id, evaluated_at DESC);

-- Drop the JSONB payload
ALTER TABLE campaign_evaluations DROP COLUMN results;

-- New enum for per-pair baseline provenance
CREATE TYPE pair_detail_direct_source AS ENUM ('active_probe', 'vm_continuous');

CREATE TABLE campaign_evaluation_candidates (
    evaluation_id            UUID    NOT NULL REFERENCES campaign_evaluations(id) ON DELETE CASCADE,
    destination_ip           INET    NOT NULL,
    display_name             TEXT,
    city                     TEXT,
    country_code             TEXT,
    asn                      BIGINT,
    network_operator         TEXT,
    is_mesh_member           BOOLEAN NOT NULL,
    pairs_improved           INTEGER NOT NULL,
    pairs_total_considered   INTEGER NOT NULL,
    avg_improvement_ms       REAL,
    PRIMARY KEY (evaluation_id, destination_ip)
);

CREATE TABLE campaign_evaluation_pair_details (
    evaluation_id            UUID    NOT NULL,
    candidate_destination_ip INET    NOT NULL,
    source_agent_id          TEXT    NOT NULL,
    destination_agent_id     TEXT    NOT NULL,
    direct_rtt_ms            REAL    NOT NULL,
    direct_stddev_ms         REAL    NOT NULL,
    direct_loss_ratio        REAL    NOT NULL,
    direct_source            pair_detail_direct_source NOT NULL,
    transit_rtt_ms           REAL    NOT NULL,
    transit_stddev_ms        REAL    NOT NULL,
    transit_loss_ratio       REAL    NOT NULL,
    improvement_ms           REAL    NOT NULL,
    qualifies                BOOLEAN NOT NULL,
    mtr_measurement_id_ax    BIGINT  REFERENCES measurements(id) ON DELETE SET NULL,
    mtr_measurement_id_xb    BIGINT  REFERENCES measurements(id) ON DELETE SET NULL,
    PRIMARY KEY (evaluation_id, candidate_destination_ip, source_agent_id, destination_agent_id),
    FOREIGN KEY (evaluation_id, candidate_destination_ip)
        REFERENCES campaign_evaluation_candidates (evaluation_id, destination_ip)
        ON DELETE CASCADE
);
CREATE INDEX campaign_evaluation_pair_details_evaluation_idx
    ON campaign_evaluation_pair_details (evaluation_id);

CREATE TABLE campaign_evaluation_unqualified_reasons (
    evaluation_id  UUID NOT NULL REFERENCES campaign_evaluations(id) ON DELETE CASCADE,
    destination_ip INET NOT NULL,
    reason         TEXT NOT NULL,
    PRIMARY KEY (evaluation_id, destination_ip)
);

COMMIT;
