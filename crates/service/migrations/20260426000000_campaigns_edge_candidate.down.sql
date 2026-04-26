-- Best-effort reversal. The 'edge_candidate' enum variant cannot be cleanly
-- removed if any campaign_evaluations row references it; in that case the
-- DROP TYPE step below fails and operators must clean those rows manually.

DROP INDEX IF EXISTS campaign_eval_edge_pair_candidate_idx;
DROP INDEX IF EXISTS campaign_eval_edge_pair_eval_idx;
DROP TABLE IF EXISTS campaign_evaluation_edge_pair_details;

ALTER TABLE campaign_evaluation_candidates
  DROP COLUMN IF EXISTS has_real_x_source_data,
  DROP COLUMN IF EXISTS twohop_share,
  DROP COLUMN IF EXISTS onehop_share,
  DROP COLUMN IF EXISTS direct_share,
  DROP COLUMN IF EXISTS coverage_weighted_ping_ms,
  DROP COLUMN IF EXISTS mean_ms_under_t,
  DROP COLUMN IF EXISTS destinations_total,
  DROP COLUMN IF EXISTS coverage_count,
  DROP COLUMN IF EXISTS agent_id,
  DROP COLUMN IF EXISTS hostname,
  DROP COLUMN IF EXISTS notes,
  DROP COLUMN IF EXISTS website;

ALTER TABLE campaign_evaluation_pair_details
  DROP COLUMN IF EXISTS winning_x_position,
  DROP COLUMN IF EXISTS direct_was_substituted,
  DROP COLUMN IF EXISTS xb_was_substituted,
  DROP COLUMN IF EXISTS ax_was_substituted;

ALTER TABLE campaign_evaluations
  DROP COLUMN IF EXISTS vm_lookback_minutes,
  DROP COLUMN IF EXISTS max_hops,
  DROP COLUMN IF EXISTS useful_latency_ms;

ALTER TABLE measurement_campaigns
  DROP CONSTRAINT IF EXISTS measurement_campaigns_vm_lookback_range,
  DROP CONSTRAINT IF EXISTS measurement_campaigns_max_hops_range,
  DROP CONSTRAINT IF EXISTS measurement_campaigns_useful_latency_ms_positive;

ALTER TABLE measurement_campaigns
  DROP COLUMN IF EXISTS vm_lookback_minutes,
  DROP COLUMN IF EXISTS max_hops,
  DROP COLUMN IF EXISTS useful_latency_ms;

-- Postgres has no ALTER TYPE ... DROP VALUE; the variant stays in the type.
-- Documented limitation; safe for fresh dev rollback (no rows reference it yet).
