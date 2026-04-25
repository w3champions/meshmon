BEGIN;

-- T55 review fix (P2-1): persist the candidate-level `avg_loss_ratio`
-- on `campaign_evaluation_candidates` instead of recomputing it at read
-- time from `campaign_evaluation_pair_details`. The evaluator already
-- builds it from the pre-storage-filter `compound_losses` accumulator,
-- but the read path used to recompute via `LEFT JOIN ... AVG(pd.transit_loss_ratio)`,
-- which only sees the rows the storage filter let through. Storing the
-- evaluator's value gives the headline a stable reading regardless of
-- how aggressively the storage floors prune detail rows.
--
-- Nullable: a candidate with zero compound-loss samples (no eligible
-- triples after the loss-threshold gate) still produces a row, with
-- `pairs_total_considered = 0` skipped earlier — but a partially-empty
-- candidate could in principle land here with no losses recorded.
-- NULL preserves the pre-existing read-path semantics.
ALTER TABLE campaign_evaluation_candidates
    ADD COLUMN avg_loss_ratio REAL;

-- T55 review fix (P2-2): persist the qualifying-leg set independently of
-- the storage filter. `Detail: good candidates` expands a candidate's
-- qualifying triples into measurement targets — when both
-- `min_improvement_ms` and `min_improvement_ratio` were unset the old
-- path could walk `campaign_evaluation_pair_details WHERE qualifies = true`
-- safely, but a tight storage floor drops `qualifies = true` rows from
-- that table and leaves the dispatch under-pointed. The new table is
-- written by the evaluator from the same accumulator that drives
-- `pairs_improved`, so the dispatch sees every qualifying triple.
CREATE TABLE campaign_evaluation_qualifying_legs (
    evaluation_id            UUID NOT NULL,
    candidate_destination_ip INET NOT NULL,
    source_agent_id          TEXT NOT NULL,
    destination_agent_id     TEXT NOT NULL,
    PRIMARY KEY (evaluation_id, candidate_destination_ip, source_agent_id, destination_agent_id),
    FOREIGN KEY (evaluation_id, candidate_destination_ip)
        REFERENCES campaign_evaluation_candidates (evaluation_id, destination_ip)
        ON DELETE CASCADE
);
CREATE INDEX campaign_evaluation_qualifying_legs_evaluation_idx
    ON campaign_evaluation_qualifying_legs (evaluation_id);

COMMIT;
