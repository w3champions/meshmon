ALTER TABLE campaign_pairs
    DROP CONSTRAINT campaign_pairs_campaign_id_source_kind_dest_key;

ALTER TABLE campaign_pairs
    ADD CONSTRAINT campaign_pairs_campaign_id_source_agent_id_destination_ip_key
        UNIQUE (campaign_id, source_agent_id, destination_ip);
