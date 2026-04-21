import type { Campaign } from "@/api/hooks/campaigns";

/**
 * Stub — the Re-evaluate form (PATCH knobs → POST /evaluate) lands in the
 * next task in this batch. Exists now so the tab shell can conditionally
 * mount the active panel.
 */
export interface SettingsTabProps {
  campaign: Campaign;
}

export function SettingsTab({ campaign }: SettingsTabProps) {
  return (
    <section
      data-testid="settings-tab"
      role="status"
      className="rounded-md border border-dashed p-4 text-sm text-muted-foreground"
      aria-label="Settings tab"
    >
      Evaluation settings — coming next (campaign {campaign.id}).
    </section>
  );
}
