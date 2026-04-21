-- Widen the campaign_pairs uniqueness key to include `kind` so that a
-- single (source, destination) can carry both detail_ping and detail_mtr
-- rows alongside the original campaign row.

ALTER TABLE campaign_pairs
    DROP CONSTRAINT campaign_pairs_campaign_id_source_agent_id_destination_ip_key;

ALTER TABLE campaign_pairs
    ADD CONSTRAINT campaign_pairs_campaign_id_source_kind_dest_key
        UNIQUE (campaign_id, source_agent_id, destination_ip, kind);
