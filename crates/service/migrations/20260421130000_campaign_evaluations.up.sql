-- Adds the per-campaign evaluation artefact and the kind discriminator on
-- campaign_pairs. The kind on campaign_pairs mirrors measurements.kind so
-- detail runs are trivially excluded from the evaluator's baseline/candidate
-- matrix.

ALTER TABLE campaign_pairs
  ADD COLUMN kind measurement_kind NOT NULL DEFAULT 'campaign';

CREATE INDEX campaign_pairs_campaign_kind_idx
  ON campaign_pairs (campaign_id, kind);

CREATE TABLE campaign_evaluations (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    campaign_id          UUID NOT NULL UNIQUE
                              REFERENCES measurement_campaigns(id) ON DELETE CASCADE,
    evaluated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    loss_threshold_pct   REAL NOT NULL,
    stddev_weight        REAL NOT NULL,
    evaluation_mode      evaluation_mode NOT NULL,
    baseline_pair_count  INTEGER NOT NULL,
    candidates_total     INTEGER NOT NULL,
    candidates_good      INTEGER NOT NULL,
    avg_improvement_ms   REAL,
    results              JSONB NOT NULL
);
