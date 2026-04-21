/**
 * Per-candidate drilldown drawer for the Candidates tab.
 *
 * Opens as a right-side sheet listing each pair the evaluator scored for
 * the selected transit candidate. Each pair row shows the direct A→B
 * metrics, the composed A→X→B transit metrics, the signed improvement, and
 * — when the evaluator stored MTR measurement ids — buttons that lazily
 * load the joined `mtr_hops` via the campaign-measurements endpoint and
 * render them into the existing `RouteTopology` component.
 */

import { useMemo, useState } from "react";
import type { Campaign } from "@/api/hooks/campaigns";
import { useCampaignMeasurements } from "@/api/hooks/campaigns";
import { useAgents, type AgentSummary } from "@/api/hooks/agents";
import type { Evaluation } from "@/api/hooks/evaluation";
import { RouteTopology } from "@/components/RouteTopology";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { cn } from "@/lib/utils";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type Candidate = Evaluation["results"]["candidates"][number];
type PairDetail = Candidate["pair_details"][number];

export interface DrilldownDrawerProps {
  candidate: Candidate | null;
  campaign: Campaign;
  onClose: () => void;
  /**
   * Unqualified-reason map off `EvaluationResultsDto.unqualified_reasons`;
   * rendered verbatim under the candidate header when present.
   */
  unqualifiedReason?: string;
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

function formatMs(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  return `${value.toFixed(1)} ms`;
}

function formatLoss(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  return `${value.toFixed(2)}%`;
}

function formatImprovement(value: number): string {
  const rounded = Math.round(value * 10) / 10;
  const sign = rounded > 0 ? "+" : "";
  return `${sign}${rounded.toFixed(1)} ms`;
}

function improvementClass(value: number): string {
  if (value > 0) return "text-emerald-600 dark:text-emerald-400 font-medium";
  if (value < 0) return "text-destructive font-medium";
  return "text-muted-foreground";
}

function agentLabel(agent: AgentSummary | undefined, fallback: string): string {
  if (!agent) return fallback;
  return agent.display_name || agent.id;
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function DrilldownDrawer({
  candidate,
  campaign,
  onClose,
  unqualifiedReason,
}: DrilldownDrawerProps) {
  const open = candidate !== null;

  // Agent roster is used to resolve `destination_agent_id` into a display
  // name + IP (plan §Task 14: "NOT from this row's destination_ip"). The
  // roster is already cached by other pages, so this hook is effectively
  // free after the first page mount.
  const agentsQuery = useAgents();

  const agentsById = useMemo<Map<string, AgentSummary>>(() => {
    const map = new Map<string, AgentSummary>();
    for (const agent of agentsQuery.data ?? []) {
      map.set(agent.id, agent);
    }
    return map;
  }, [agentsQuery.data]);

  return (
    <Sheet open={open} onOpenChange={(next) => !next && onClose()}>
      <SheetContent
        side="right"
        className="w-full max-w-3xl sm:max-w-3xl overflow-y-auto"
        aria-describedby={undefined}
      >
        {candidate ? (
          <CandidateBody
            candidate={candidate}
            campaign={campaign}
            agentsById={agentsById}
            unqualifiedReason={unqualifiedReason}
          />
        ) : null}
      </SheetContent>
    </Sheet>
  );
}

// ---------------------------------------------------------------------------
// Body
// ---------------------------------------------------------------------------

interface CandidateBodyProps {
  candidate: Candidate;
  campaign: Campaign;
  agentsById: Map<string, AgentSummary>;
  unqualifiedReason: string | undefined;
}

function CandidateBody({ candidate, campaign, agentsById, unqualifiedReason }: CandidateBodyProps) {
  const [activeMtr, setActiveMtr] = useState<{
    pairKey: string;
    measurementId: number;
    label: string;
  } | null>(null);

  return (
    <>
      <SheetHeader>
        <SheetTitle>
          {candidate.display_name ?? candidate.destination_ip}
          {candidate.is_mesh_member ? (
            <Badge variant="secondary" className="ml-2" aria-label="Mesh member">
              mesh
            </Badge>
          ) : null}
        </SheetTitle>
        <SheetDescription>
          Transit candidate {candidate.destination_ip} — {candidate.pairs_improved} of{" "}
          {candidate.pairs_total_considered} baseline pairs improved.
        </SheetDescription>
      </SheetHeader>

      {unqualifiedReason ? (
        <Card className="mt-4 border-amber-500/50 bg-amber-500/5 p-3 text-sm" role="status">
          <span className="font-medium">Unqualified:</span> {unqualifiedReason}
        </Card>
      ) : null}

      <section className="mt-4 flex flex-col gap-3">
        <h3 className="text-sm font-semibold">Per-pair scoring</h3>
        {candidate.pair_details.length === 0 ? (
          <Card className="p-4 text-sm text-muted-foreground" role="status">
            The evaluator reported no pair-level scoring for this candidate.
          </Card>
        ) : (
          <ul className="flex flex-col gap-2" aria-label="Pair scoring rows">
            {candidate.pair_details.map((pair, idx) => {
              const pairKey = `${pair.source_agent_id}→${pair.destination_agent_id}::${idx}`;
              return (
                <PairRow
                  key={pairKey}
                  pair={pair}
                  pairKey={pairKey}
                  sourceAgent={agentsById.get(pair.source_agent_id)}
                  destAgent={agentsById.get(pair.destination_agent_id)}
                  onOpenMtr={(measurementId, label) =>
                    setActiveMtr({ pairKey, measurementId, label })
                  }
                />
              );
            })}
          </ul>
        )}
      </section>

      {activeMtr ? (
        <MtrPanel
          campaign={campaign}
          measurementId={activeMtr.measurementId}
          label={activeMtr.label}
          onClose={() => setActiveMtr(null)}
        />
      ) : null}
    </>
  );
}

// ---------------------------------------------------------------------------
// Pair row
// ---------------------------------------------------------------------------

interface PairRowProps {
  pair: PairDetail;
  pairKey: string;
  sourceAgent: AgentSummary | undefined;
  destAgent: AgentSummary | undefined;
  onOpenMtr: (measurementId: number, label: string) => void;
}

function PairRow({ pair, sourceAgent, destAgent, onOpenMtr }: PairRowProps) {
  const sourceLabel = agentLabel(sourceAgent, pair.source_agent_id);
  const destLabel = agentLabel(destAgent, pair.destination_agent_id);
  const destIp = destAgent?.ip ?? pair.destination_agent_id;

  return (
    <li>
      <Card className="flex flex-col gap-2 p-3 text-sm">
        <header className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <span className="font-medium">{sourceLabel}</span>
            <span aria-hidden className="text-muted-foreground">
              →
            </span>
            <span className="font-medium">{destLabel}</span>
            <span className="font-mono text-xs text-muted-foreground">({destIp})</span>
          </div>
          <div className="flex items-center gap-2">
            {pair.qualifies ? (
              <Badge
                variant="secondary"
                className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
              >
                qualifies
              </Badge>
            ) : (
              <Badge variant="outline">below gate</Badge>
            )}
            <span className={cn("tabular-nums", improvementClass(pair.improvement_ms))}>
              {formatImprovement(pair.improvement_ms)}
            </span>
          </div>
        </header>

        <dl className="grid grid-cols-2 gap-3 text-xs text-muted-foreground sm:grid-cols-3">
          <MetricBlock
            label="Direct A→B"
            rtt={pair.direct_rtt_ms}
            stddev={pair.direct_stddev_ms}
            loss={pair.direct_loss_pct}
          />
          <MetricBlock
            label={`Transit via ${pair.destination_ip}`}
            rtt={pair.transit_rtt_ms}
            stddev={pair.transit_stddev_ms}
            loss={pair.transit_loss_pct}
          />
        </dl>

        <footer className="flex flex-wrap items-center gap-2">
          <MtrLinkButton
            measurementId={pair.mtr_measurement_id_ax}
            label={`MTR ${sourceLabel} → ${pair.destination_ip}`}
            onOpen={onOpenMtr}
          />
          <MtrLinkButton
            measurementId={pair.mtr_measurement_id_xb}
            label={`MTR ${pair.destination_ip} → ${destLabel}`}
            onOpen={onOpenMtr}
          />
        </footer>
      </Card>
    </li>
  );
}

interface MetricBlockProps {
  label: string;
  rtt: number;
  stddev: number;
  loss: number;
}

function MetricBlock({ label, rtt, stddev, loss }: MetricBlockProps) {
  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-xs uppercase tracking-wide">{label}</dt>
      <dd className="font-mono text-sm text-foreground tabular-nums">
        {formatMs(rtt)} <span className="text-xs text-muted-foreground">±{formatMs(stddev)}</span>
      </dd>
      <dd className="text-xs text-muted-foreground">loss {formatLoss(loss)}</dd>
    </div>
  );
}

interface MtrLinkButtonProps {
  measurementId: number | null | undefined;
  label: string;
  onOpen: (measurementId: number, label: string) => void;
}

function MtrLinkButton({ measurementId, label, onOpen }: MtrLinkButtonProps) {
  if (measurementId === null || measurementId === undefined) {
    return (
      <Button type="button" size="sm" variant="ghost" disabled aria-label={`${label} (unavailable)`}>
        {label.split(" ")[0]} n/a
      </Button>
    );
  }
  return (
    <Button
      type="button"
      size="sm"
      variant="outline"
      onClick={() => onOpen(measurementId, label)}
      aria-label={label}
    >
      {label}
    </Button>
  );
}

// ---------------------------------------------------------------------------
// MTR panel — lazy single-row measurements fetch → RouteTopology
// ---------------------------------------------------------------------------

interface MtrPanelProps {
  campaign: Campaign;
  measurementId: number;
  label: string;
  onClose: () => void;
}

function MtrPanel({ campaign, measurementId, label, onClose }: MtrPanelProps) {
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
