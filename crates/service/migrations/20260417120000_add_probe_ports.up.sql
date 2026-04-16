-- Add echo-listener probe ports advertised at registration and returned in
-- TargetsResponse. Safe to run NOT NULL from the start because meshmon has
-- no deployed instances yet; if that changes, split into nullable ADD +
-- backfill + SET NOT NULL.

ALTER TABLE agents
    ADD COLUMN tcp_probe_port INTEGER NOT NULL,
    ADD COLUMN udp_probe_port INTEGER NOT NULL;

ALTER TABLE agents
    ADD CONSTRAINT tcp_probe_port_range CHECK (tcp_probe_port BETWEEN 1 AND 65535),
    ADD CONSTRAINT udp_probe_port_range CHECK (udp_probe_port BETWEEN 1 AND 65535);
