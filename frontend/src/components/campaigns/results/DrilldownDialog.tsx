/**
 * Centered drilldown dialog for the Candidates tab.
 *
 * Centered modal (`max-w-6xl`, `max-h-[85vh]`, internal scroll). Branches on
 * `evaluation.evaluation_mode`:
 * - `edge_candidate` → `<EdgePairDrawerBody>` + provenance chips
 * - else (diversity/optimization) → `<TripleDrawerBody>` (paginated pair-detail)
 *
 * Header: `<CandidateRef mode="header">` replaces the bare name + IP text
 * from the pre-M3 implementation.
 *
 * Toolbar (Qualifies-only toggle, Sort, Reset) persists to
 * `localStorage[meshmon.evaluation.drawer.{mode}]` (per-mode globally).
 */

import type { Campaign } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";
import { CandidateRef } from "@/components/campaigns/CandidateRef";
import { EdgePairDrawerBody } from "@/components/campaigns/results/EdgePairDrawerBody";
import { TripleDrawerBody } from "@/components/campaigns/results/TripleDrawerBody";
import { summarizeGuardrails } from "@/components/campaigns/results/TripleDrawerBody";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type Candidate = Evaluation["results"]["candidates"][number];

export interface DrilldownDialogProps {
  candidate: Candidate | null;
  campaign: Campaign;
  /**
   * Latest evaluation snapshot. Carries the active guardrail values
   * (rendered as input placeholders) and the candidate's headline
   * counters used by the caption math.
   */
  evaluation: Evaluation | null;
  /**
   * Unqualified-reason map off `EvaluationResultsDto.unqualified_reasons`;
   * rendered verbatim under the candidate header when present.
   */
  unqualifiedReason?: string;
  onClose: () => void;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function DrilldownDialog({
  candidate,
  campaign,
  evaluation,
  unqualifiedReason,
  onClose,
}: DrilldownDialogProps) {
  const open = candidate !== null;
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="flex max-h-[85vh] max-w-6xl flex-col gap-0 overflow-hidden p-0 sm:rounded-lg">
        {candidate ? (
          <DialogBody
            candidate={candidate}
            campaign={campaign}
            evaluation={evaluation}
            unqualifiedReason={unqualifiedReason}
            onClose={onClose}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// Body
// ---------------------------------------------------------------------------

interface DialogBodyProps {
  candidate: Candidate;
  campaign: Campaign;
  evaluation: Evaluation | null;
  unqualifiedReason: string | undefined;
  onClose: () => void;
}

function DialogBody({
  candidate,
  campaign,
  evaluation,
  unqualifiedReason,
  onClose,
}: DialogBodyProps) {
  const isEdgeMode = evaluation?.evaluation_mode === "edge_candidate";

  const guardrails = {
    min_improvement_ms: evaluation?.min_improvement_ms ?? null,
    min_improvement_ratio: evaluation?.min_improvement_ratio ?? null,
    max_transit_rtt_ms: evaluation?.max_transit_rtt_ms ?? null,
    max_transit_stddev_ms: evaluation?.max_transit_stddev_ms ?? null,
  };

  const guardrailActive =
    guardrails.min_improvement_ms !== null ||
    guardrails.min_improvement_ratio !== null ||
    guardrails.max_transit_rtt_ms !== null ||
    guardrails.max_transit_stddev_ms !== null;

  // Build CandidateRef data from the candidate DTO
  const candidateRefData = {
    ip: candidate.destination_ip,
    display_name: candidate.display_name,
    city: candidate.city,
    country_code: candidate.country_code,
    asn: candidate.asn,
    network_operator: candidate.network_operator,
    hostname: candidate.hostname,
    is_mesh_member: candidate.is_mesh_member,
    agent_id: (candidate as Candidate & { agent_id?: string | null }).agent_id,
  };

  return (
    <>
      <DialogHeader className="border-b px-6 pb-4 pt-5">
        <DialogTitle asChild>
          <div>
            <CandidateRef mode="header" data={candidateRefData} />
          </div>
        </DialogTitle>

        <DialogDescription asChild>
          <div className="flex flex-wrap items-center gap-3 text-sm text-muted-foreground mt-2">
            {isEdgeMode ? (
              /* Edge-candidate: show coverage stats */
              <span>
                Edge candidate{" "}
                {candidate.destination_ip}
              </span>
            ) : (
              /* Triple: show pair stats */
              <>
                <span>
                  Transit candidate{" "}
                  <span className="font-mono">{candidate.destination_ip}</span>
                </span>
                <span aria-hidden>·</span>
                <span>
                  <strong className="tabular-nums">{candidate.pairs_improved}</strong> of{" "}
                  <strong className="tabular-nums">{candidate.pairs_total_considered}</strong>{" "}
                  baseline pairs improved
                </span>
                {guardrailActive ? (
                  <Badge
                    variant="outline"
                    className="font-mono text-[10px]"
                    aria-label="Active guardrails"
                  >
                    {summarizeGuardrails(guardrails)}
                  </Badge>
                ) : null}
              </>
            )}
          </div>
        </DialogDescription>

        {/* Provenance chips — edge_candidate mode only (per plan M3 lines 3095-3101) */}
        {isEdgeMode ? (
          <ProvenanceChips candidate={candidate} />
        ) : null}

        {unqualifiedReason ? (
          <Card className="border-amber-500/50 bg-amber-500/5 p-3 text-sm" role="status">
            <span className="font-medium">Unqualified:</span> {unqualifiedReason}
          </Card>
        ) : null}
      </DialogHeader>

      {isEdgeMode ? (
        <EdgePairDrawerBody
          candidateIp={candidate.destination_ip}
          candidate={candidate}
          campaign={campaign}
        />
      ) : (
        <TripleDrawerBody
          candidate={candidate}
          campaign={campaign}
          evaluation={evaluation}
          unqualifiedReason={unqualifiedReason}
          onClose={onClose}
        />
      )}
    </>
  );
}

// ---------------------------------------------------------------------------
// Provenance chips (edge_candidate mode only)
// ---------------------------------------------------------------------------

/**
 * Provenance chips for edge_candidate mode (per plan M3 / brainstorm Q-final).
 *
 * - has_real_x_source_data === true →
 *   green "Real X-source data — no symmetry approximation"
 * - is_mesh_member === false (every leg symmetric reuse) →
 *   gray "Symmetric-reuse approximation (no agent at this candidate)"
 * - is_mesh_member === true && has_real_x_source_data === false →
 *   yellow "Symmetric-reuse approximation (mesh agent — VM data unavailable)"
 */
function ProvenanceChips({ candidate }: { candidate: Candidate }) {
  const hasReal =
    (candidate as Candidate & { has_real_x_source_data?: boolean | null }).has_real_x_source_data;

  if (hasReal === true) {
    return (
      <div className="flex flex-wrap gap-2 mt-1">
        <Badge
          variant="outline"
          className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
          data-testid="provenance-real"
        >
          Real X-source data — no symmetry approximation
        </Badge>
      </div>
    );
  }

  if (!candidate.is_mesh_member) {
    return (
      <div className="flex flex-wrap gap-2 mt-1">
        <Badge
          variant="outline"
          className="bg-muted text-muted-foreground"
          data-testid="provenance-sym-no-agent"
        >
          Symmetric-reuse approximation (no agent at this candidate)
        </Badge>
      </div>
    );
  }

  // is_mesh_member === true && has_real_x_source_data === false/null/undefined
  return (
    <div className="flex flex-wrap gap-2 mt-1">
      <Badge
        variant="outline"
        className="bg-amber-500/15 text-amber-700 dark:text-amber-300"
        data-testid="provenance-sym-mesh"
      >
        Symmetric-reuse approximation (mesh agent — VM data unavailable)
      </Badge>
    </div>
  );
}
