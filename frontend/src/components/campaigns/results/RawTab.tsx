import type { Campaign } from "@/api/hooks/campaigns";

/**
 * Stub — the raw-measurements browser (virtualised table + filter bar over
 * `campaign_measurements`) lands in Batch 5 (Tasks 17–18). Exists now so the
 * tab shell can conditionally mount the active panel.
 */
export interface RawTabProps {
  campaign: Campaign;
}

export function RawTab({ campaign }: RawTabProps) {
  return (
    <section
      data-testid="raw-tab"
      role="status"
      className="rounded-md border border-dashed p-4 text-sm text-muted-foreground"
      aria-label="Raw measurements tab"
    >
      Raw measurements tab — coming in Batch 5 (campaign {campaign.id}).
    </section>
  );
}
