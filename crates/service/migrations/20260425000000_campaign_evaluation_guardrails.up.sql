BEGIN;

-- Optional evaluator guardrail knobs. All four columns are nullable
-- with no DEFAULT, so historic rows backfill to NULL (knob disabled).
--
-- `max_transit_rtt_ms` and `max_transit_stddev_ms` are eligibility caps
-- the evaluator applies before counter accumulation; `min_improvement_ms`
-- and `min_improvement_ratio` are storage floors the evaluator applies
-- after counter accumulation (OR semantics across the two floors).
ALTER TABLE measurement_campaigns
    ADD COLUMN max_transit_rtt_ms      DOUBLE PRECISION,
    ADD COLUMN max_transit_stddev_ms   DOUBLE PRECISION,
    ADD COLUMN min_improvement_ms      DOUBLE PRECISION,
    ADD COLUMN min_improvement_ratio   DOUBLE PRECISION;

-- Snapshot the same four values on `campaign_evaluations` at /evaluate
-- time so each evaluation row carries the guardrails that produced it.
ALTER TABLE campaign_evaluations
    ADD COLUMN max_transit_rtt_ms      DOUBLE PRECISION,
    ADD COLUMN max_transit_stddev_ms   DOUBLE PRECISION,
    ADD COLUMN min_improvement_ms      DOUBLE PRECISION,
    ADD COLUMN min_improvement_ratio   DOUBLE PRECISION;

COMMIT;
