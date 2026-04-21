import type { Campaign } from "@/api/hooks/campaigns";

/**
 * Stub — the real candidate browser (evaluation results table + drilldown
 * drawer + KPI row) lands in Batch 4 (Tasks 13–15). Exists now so the tab
 * shell (`CampaignDetail.tsx`) can conditionally mount the active panel.
 */
export interface CandidatesTabProps {
  campaign: Campaign;
}

export function CandidatesTab({ campaign }: CandidatesTabProps) {
  return (
    <section
      data-testid="candidates-tab"
      role="status"
      className="rounded-md border border-dashed p-4 text-sm text-muted-foreground"
      aria-label="Candidates tab"
    >
      Candidates tab — coming in Batch 4 (campaign {campaign.id}).
    </section>
  );
}
