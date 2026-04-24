-- T49+: distinguish how a measurement row got into the table.
--   * `active_probe` — the campaign dispatched an agent to probe. Default.
--   * `archived_vm_continuous` — the evaluator fetched a baseline from
--     VictoriaMetrics continuous mesh data and archived it so raw
--     measurement rows remain the authoritative record.

CREATE TYPE measurement_source AS ENUM ('active_probe', 'archived_vm_continuous');

ALTER TABLE measurements
    ADD COLUMN source measurement_source NOT NULL DEFAULT 'active_probe';

-- Lookup for baseline rows by agent pair + recency during evaluation.
CREATE INDEX measurements_vm_baseline_idx
    ON measurements (source_agent_id, destination_ip, protocol, measured_at DESC)
    WHERE source = 'archived_vm_continuous';
