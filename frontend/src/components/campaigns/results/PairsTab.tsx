import type { Campaign } from "@/api/hooks/campaigns";

/**
 * Stub — the pair table + row actions (per-pair Detail, force-remeasure,
 * open MTR) land in Batch 5 (Task 16). Exists now so the tab shell can
 * conditionally mount the active panel.
 */
export interface PairsTabProps {
  campaign: Campaign;
}

export function PairsTab({ campaign }: PairsTabProps) {
  return (
    <section
      data-testid="pairs-tab"
      role="status"
      className="rounded-md border border-dashed p-4 text-sm text-muted-foreground"
      aria-label="Pairs tab"
    >
      Pairs tab — coming in Batch 5 (campaign {campaign.id}).
    </section>
  );
}
