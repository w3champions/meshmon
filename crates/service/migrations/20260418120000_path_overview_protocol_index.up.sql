-- Covering index for `DISTINCT ON (protocol)` in the path-overview
-- `latest_by_protocol` query. The pre-existing `idx_route_snapshots_lookup`
-- leads with `(source_id, target_id, observed_at DESC)`, which does not
-- satisfy the `ORDER BY protocol, observed_at DESC` Postgres needs to
-- evaluate `DISTINCT ON (protocol)` without an in-memory sort. Adding
-- `protocol` as the third key lets the planner walk the index directly
-- and emit one row per (source, target, protocol) triple.
CREATE INDEX IF NOT EXISTS idx_route_snapshots_protocol_lookup
    ON route_snapshots (source_id, target_id, protocol, observed_at DESC);
