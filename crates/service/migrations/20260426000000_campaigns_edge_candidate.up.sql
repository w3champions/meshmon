-- Edge candidate evaluation mode + cross-mode improvements (T56).
-- Spec: docs/superpowers/specs/2026-04-26-campaigns-edge-candidate-evaluation-mode-design.md

-- 1. New enum variant.
ALTER TYPE evaluation_mode ADD VALUE 'edge_candidate' AFTER 'optimization';

-- 2. Three new knobs on measurement_campaigns.
ALTER TABLE measurement_campaigns
  ADD COLUMN useful_latency_ms    REAL    NULL,
  ADD COLUMN max_hops             SMALLINT NOT NULL DEFAULT 2,
  ADD COLUMN vm_lookback_minutes  INTEGER  NOT NULL DEFAULT 15;

ALTER TABLE measurement_campaigns
  ADD CONSTRAINT measurement_campaigns_useful_latency_ms_positive
    CHECK (useful_latency_ms IS NULL OR useful_latency_ms > 0),
  ADD CONSTRAINT measurement_campaigns_max_hops_range
    CHECK (max_hops BETWEEN 0 AND 2),
  ADD CONSTRAINT measurement_campaigns_vm_lookback_range
    CHECK (vm_lookback_minutes BETWEEN 1 AND 1440);

-- 3. Snapshot columns on campaign_evaluations — nullable on purpose so pre-fix
--    evaluation rows do not lie about behaviour they never had.
ALTER TABLE campaign_evaluations
  ADD COLUMN useful_latency_ms    REAL    NULL,
  ADD COLUMN max_hops             SMALLINT NULL,
  ADD COLUMN vm_lookback_minutes  INTEGER  NULL;

-- 4. Substitution flags + winning X-position on the existing pair_details (cross-cutting).
--    winning_x_position = 1 (X first) or 2 (X second) for 2-hop diversity/optimization;
--    NULL for 1-hop or direct routes where the position question doesn't apply.
ALTER TABLE campaign_evaluation_pair_details
  ADD COLUMN ax_was_substituted     BOOLEAN  NULL,
  ADD COLUMN xb_was_substituted     BOOLEAN  NULL,
  ADD COLUMN direct_was_substituted BOOLEAN  NULL,
  ADD COLUMN winning_x_position     SMALLINT NULL;

-- 5. Per-(X, B) result table for edge_candidate.
CREATE TABLE campaign_evaluation_edge_pair_details (
  id                        BIGSERIAL PRIMARY KEY,
  evaluation_id             UUID NOT NULL REFERENCES campaign_evaluations(id) ON DELETE CASCADE,
  candidate_ip              INET NOT NULL,
  destination_agent_id      TEXT NOT NULL,
  best_route_ms             REAL NULL,
  best_route_loss_ratio     REAL NOT NULL,
  best_route_stddev_ms      REAL NOT NULL,
  best_route_kind           TEXT NOT NULL,
  best_route_intermediaries TEXT[] NOT NULL DEFAULT '{}',
  best_route_legs           JSONB NOT NULL,
  qualifies_under_t         BOOLEAN NOT NULL,
  is_unreachable            BOOLEAN NOT NULL DEFAULT FALSE,
  CONSTRAINT campaign_eval_edge_pair_kind
    CHECK (best_route_kind IN ('direct', '1hop', '2hop'))
);

CREATE INDEX campaign_eval_edge_pair_eval_idx
  ON campaign_evaluation_edge_pair_details (evaluation_id);
CREATE INDEX campaign_eval_edge_pair_candidate_idx
  ON campaign_evaluation_edge_pair_details (evaluation_id, candidate_ip);

-- 6. Edge-mode aggregate + enrichment columns on existing candidates table.
--    The enrichment columns (website, notes, agent_id) are written by the
--    EdgeCandidate evaluator arm; they are NULL for pre-T56 rows.
--    hostname is intentionally NOT persisted here — it is stamped at
--    response time from ip_hostname_cache via bulk_hostnames_and_enqueue,
--    matching the invariant asserted by the
--    get_evaluation_stamps_candidate_and_pair_detail_hostnames test.
ALTER TABLE campaign_evaluation_candidates
  ADD COLUMN website                   TEXT    NULL,
  ADD COLUMN notes                     TEXT    NULL,
  ADD COLUMN agent_id                  TEXT    NULL,
  ADD COLUMN coverage_count            INTEGER NULL,
  ADD COLUMN destinations_total        INTEGER NULL,
  ADD COLUMN mean_ms_under_t           REAL    NULL,
  ADD COLUMN coverage_weighted_ping_ms REAL    NULL,
  ADD COLUMN direct_share              REAL    NULL,
  ADD COLUMN onehop_share              REAL    NULL,
  ADD COLUMN twohop_share              REAL    NULL,
  ADD COLUMN has_real_x_source_data    BOOLEAN NULL;
