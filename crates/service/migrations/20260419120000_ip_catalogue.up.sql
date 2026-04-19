-- T42: IP catalogue + enrichment pipeline.
-- Introduces the single authoritative table for every IP meshmon knows
-- about (operator-added or agent-derived). Moves geo coordinates off
-- `agents` and onto the catalogue; `agents.location` stays as free-text
-- metadata.

-- gen_random_uuid() is built into Postgres 13+ (no pgcrypto extension
-- required). The shared test container runs pg16 (timescale/timescaledb:2.26.3-pg16).

-- Enums
CREATE TYPE enrichment_status AS ENUM ('pending', 'enriched', 'failed');
CREATE TYPE catalogue_source  AS ENUM ('operator', 'agent_registration');

-- Table
CREATE TABLE ip_catalogue (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    ip                      INET NOT NULL UNIQUE,
    display_name            TEXT,
    city                    TEXT,
    country_code            CHAR(2),
    country_name            TEXT,
    latitude                DOUBLE PRECISION,
    longitude               DOUBLE PRECISION,
    asn                     INTEGER,
    network_operator        TEXT,
    website                 TEXT,
    notes                   TEXT,
    enrichment_status       enrichment_status NOT NULL DEFAULT 'pending',
    enriched_at             TIMESTAMPTZ,
    operator_edited_fields  TEXT[] NOT NULL DEFAULT '{}',
    source                  catalogue_source NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_by              TEXT
);

-- Indexes
CREATE INDEX idx_ip_catalogue_country ON ip_catalogue (country_code);
CREATE INDEX idx_ip_catalogue_asn     ON ip_catalogue (asn);
CREATE INDEX idx_ip_catalogue_latlon  ON ip_catalogue (latitude, longitude);

-- Partial index supporting the enrichment runner sweep. The sweep runs
-- every 30 s with
--   SELECT id FROM ip_catalogue
--   WHERE enrichment_status = 'pending' AND created_at < NOW() - INTERVAL '30 seconds'
--   ORDER BY created_at ASC, id ASC LIMIT 128
-- so a partial `(created_at, id)` index restricted to `pending` rows
-- keeps the index tiny (most rows settle into `enriched` / `failed`)
-- while still ordering the hot set. Without this, the sweep degenerates
-- into sequential scans as the catalogue grows.
CREATE INDEX idx_ip_catalogue_pending_sweep
    ON ip_catalogue (created_at, id)
    WHERE enrichment_status = 'pending';

-- Full-text search across every filterable text field (notes excluded).
CREATE INDEX idx_ip_catalogue_search ON ip_catalogue USING GIN (
    to_tsvector(
        'simple',
        coalesce(display_name,'')       || ' ' ||
        coalesce(city,'')               || ' ' ||
        coalesce(country_name,'')       || ' ' ||
        coalesce(network_operator,'')
    )
);

-- Backfill any existing agent coordinates into the catalogue *before*
-- dropping the columns. Without this, upgrading a live deployment
-- silently loses every registered agent's lat/lon until each agent
-- re-registers — and the registry view `agents_with_catalogue` reads
-- geo from `ip_catalogue`, so the API would serve empty coordinates
-- in the interim. `Latitude` / `Longitude` go straight into
-- `operator_edited_fields` so the enrichment chain will not overwrite
-- the agent-reported position.
INSERT INTO ip_catalogue (ip, source, latitude, longitude, operator_edited_fields)
SELECT a.ip,
       'agent_registration'::catalogue_source,
       a.lat,
       a.lon,
       ARRAY['Latitude', 'Longitude']::text[]
FROM agents a
WHERE a.lat IS NOT NULL AND a.lon IS NOT NULL
ON CONFLICT (ip) DO NOTHING;

-- Agents -> catalogue: geo lives on the catalogue only
ALTER TABLE agents DROP COLUMN lat;
ALTER TABLE agents DROP COLUMN lon;

-- View used by the campaign composer's source filter. LEFT JOIN so
-- agents without a catalogue entry still return.
CREATE VIEW agents_with_catalogue AS
SELECT
    a.id           AS agent_id,
    a.ip           AS ip,
    a.display_name AS agent_display_name,
    a.location     AS agent_location,
    a.agent_version,
    a.registered_at,
    a.last_seen_at,
    c.id           AS catalogue_id,
    c.display_name AS catalogue_display_name,
    c.city,
    c.country_code,
    c.country_name,
    c.latitude,
    c.longitude,
    c.asn,
    c.network_operator,
    c.enrichment_status
FROM agents a
LEFT JOIN ip_catalogue c ON c.ip = a.ip;
