/**
 * MTR hop visualization panel — lazy single-row measurements fetch wired
 * into the existing `RouteTopology` component.
 *
 * Lifted out of `DrilldownDrawer.tsx` so the I5 paginated pair-details
 * dialog can consume the same component without duplicating the four-state
 * (loading / error / empty / data) card pattern. Behaviour is unchanged.
 */

import type { Campaign } from "@/api/hooks/campaigns";
import { useCampaignMeasurements } from "@/api/hooks/campaigns";
import { RouteTopology } from "@/components/RouteTopology";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";

export interface MtrPanelProps {
  campaign: Campaign;
  measurementId: number;
  label: string;
  onClose: () => void;
}

export function MtrPanel({ campaign, measurementId, label, onClose }: MtrPanelProps) {
  const measurementsQuery = useCampaignMeasurements(campaign.id, {
    measurement_id: measurementId,
    limit: 1,
  });

  const row = measurementsQuery.data?.pages[0]?.entries[0];
  const hops = row?.mtr_hops ?? null;

  return (
    <section className="mt-4 flex flex-col gap-2" aria-label="MTR hops">
      <header className="flex items-center justify-between">
        <h3 className="text-sm font-semibold">{label}</h3>
        <Button type="button" size="sm" variant="ghost" onClick={onClose}>
          Close
        </Button>
      </header>
      {measurementsQuery.isLoading ? (
        <Card className="p-3 text-sm text-muted-foreground" role="status">
          Loading MTR hops…
        </Card>
      ) : measurementsQuery.isError ? (
        <Card className="p-3 text-sm text-destructive" role="alert">
          Failed to load MTR hops: {measurementsQuery.error?.message ?? "unknown error"}
        </Card>
      ) : !row ? (
        <Card className="p-3 text-sm text-muted-foreground" role="status">
          The MTR measurement has not settled yet. Check the Raw tab for the in-flight pair.
        </Card>
      ) : !hops || hops.length === 0 ? (
        <Card className="p-3 text-sm text-muted-foreground" role="status">
          No hop data captured for this measurement.
        </Card>
      ) : (
        <div className="h-[320px]">
          <RouteTopology hops={hops} ariaLabel={`${label} hops`} className="h-full" />
        </div>
      )}
    </section>
  );
}
