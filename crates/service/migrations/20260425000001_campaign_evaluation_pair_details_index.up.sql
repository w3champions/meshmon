BEGIN;

-- Composite index tuned for the paginated pair_details endpoint
-- (`GET /api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details`).
-- Leading columns `(evaluation_id, candidate_destination_ip)` filter the
-- two FKs that anchor the page to a single (evaluation, candidate) tuple;
-- the trailing `improvement_ms DESC, source_agent_id ASC,
-- destination_agent_id ASC` matches the endpoint's default sort + tiebreak
-- so the keyset cursor walk is a forward index scan with no Sort node.
--
-- Non-default sort columns (direct_rtt_ms, transit_rtt_ms, ...) still
-- benefit from the leading filter columns and fall back to a sort step;
-- adding a per-column index for each of the ten sortable fields is not
-- worth the write amplification.
CREATE INDEX campaign_evaluation_pair_details_default_sort_idx
    ON campaign_evaluation_pair_details (
        evaluation_id,
        candidate_destination_ip,
        improvement_ms DESC,
        source_agent_id ASC,
        destination_agent_id ASC
    );

COMMIT;
