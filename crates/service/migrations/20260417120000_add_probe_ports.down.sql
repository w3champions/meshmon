ALTER TABLE agents
    DROP CONSTRAINT IF EXISTS tcp_probe_port_range,
    DROP CONSTRAINT IF EXISTS udp_probe_port_range,
    DROP COLUMN IF EXISTS tcp_probe_port,
    DROP COLUMN IF EXISTS udp_probe_port;
