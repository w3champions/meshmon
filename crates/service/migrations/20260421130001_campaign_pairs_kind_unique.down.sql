-- Rolling back collapses the 4-column uniqueness key back to 3 columns,
-- which cannot coexist with the `detail_ping` / `detail_mtr` rows that the
-- forward migration made possible (they share
-- `(campaign_id, source_agent_id, destination_ip)` with the `campaign` row).
-- Delete those detail rows before reintroducing the narrower constraint so
-- rollback against a populated DB does not fail with a UNIQUE violation.
DELETE FROM campaign_pairs WHERE kind IN ('detail_ping','detail_mtr');

ALTER TABLE campaign_pairs
    DROP CONSTRAINT campaign_pairs_campaign_id_source_kind_dest_key;

ALTER TABLE campaign_pairs
    ADD CONSTRAINT campaign_pairs_campaign_id_source_agent_id_destination_ip_key
        UNIQUE (campaign_id, source_agent_id, destination_ip);
