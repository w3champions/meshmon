/**
 * CompareTab — client-side re-aggregation of evaluation metrics over a
 * user-picked subset of source/destination agents.
 *
 * Surfaces:
 *   - Skeleton + placeholder when evaluation is null.
 *   - Agent picker multi-select with localStorage persistence.
 *   - Pick-role radio (diversity/optimization only; hidden for edge_candidate).
 *   - Candidate sub-picker (transient, URL-encoded).
 *   - Client-side re-aggregation for edge_candidate; triple-mode stub.
 *
 * Diversity/optimization aggregation is not wired — a visible stub notice
 * is rendered instead (see `CompareTripleStub`).
 */

import { useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useState } from "react";
import type { Campaign, EdgePairsQuery } from "@/api/hooks/campaigns";
import type { Evaluation, EvaluationEdgePairDetailDto } from "@/api/hooks/evaluation";
import { useEdgePairDetails } from "@/api/hooks/evaluation";
import { CandidateRef } from "@/components/campaigns/CandidateRef";
import { RouteMixBar } from "@/components/campaigns/RouteMixBar";
import {
  aggregateEdgeCandidates,
  mergeAggregateIntoCandidate,
} from "@/components/campaigns/results/CompareTab.aggregations";
import { DrilldownDialog } from "@/components/campaigns/results/DrilldownDialog";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import type { CampaignDetailSearch } from "@/router/index";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type Candidate = Evaluation["results"]["candidates"][number];
type PickRole = "both" | "source" | "destination";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STORAGE_FILTER_CAVEAT =
  "Compare reads the rows actually persisted. If you set tight min_improvement_* storage filters, sub-threshold rows are invisible here.";

const EDGE_PAIR_QUERY: EdgePairsQuery = { limit: 500 };

const CWP_TOOLTIP =
  "Coverage-weighted ping is computed against the full agent set; not recomputed in Compare. Open the Candidates tab to inspect per-candidate CWP at full-roster scope.";

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface CompareTabProps {
  campaign: Campaign;
  evaluation: Evaluation | null;
}

// ---------------------------------------------------------------------------
// Component: CompareTab
// ---------------------------------------------------------------------------

export function CompareTab({ campaign, evaluation }: CompareTabProps) {
  if (!evaluation) {
    return (
      <section data-testid="compare-tab" className="flex flex-col gap-4">
        <Card className="flex flex-col items-start gap-3 p-6" data-testid="compare-placeholder">
          <h2 className="text-base font-semibold">No evaluation yet</h2>
          <p className="text-sm text-muted-foreground">
            Evaluate first to compare candidates across a picked agent subset.
          </p>
        </Card>
      </section>
    );
  }

  return (
    <section data-testid="compare-tab" className="flex flex-col gap-4">
      <CompareView evaluation={evaluation} campaign={campaign} />
    </section>
  );
}

// ---------------------------------------------------------------------------
// CompareView — rendered only when evaluation is non-null
// ---------------------------------------------------------------------------

interface CompareViewProps {
  campaign: Campaign;
  evaluation: Evaluation;
}

function CompareView({ campaign, evaluation }: CompareViewProps) {
  const isEdgeMode = evaluation.evaluation_mode === "edge_candidate";

  // ------------------------------------------------------------------
  // URL search params (pick_role, candidates CSV)
  // ------------------------------------------------------------------

  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as CampaignDetailSearch;

  const pickRole: PickRole = (search.pick_role as PickRole | undefined) ?? "both";
  const candidateCsv: string | undefined = search.candidates;
  const pickedCandidateCsv: string | undefined = search.picked;

  const setSearchParam = useCallback(
    (updates: Partial<CampaignDetailSearch>): void => {
      const nav = navigate as unknown as (opts: {
        search: (prev: Record<string, unknown>) => Record<string, unknown>;
        replace: boolean;
      }) => void;
      nav({ search: (prev) => ({ ...prev, ...updates }), replace: true });
    },
    [navigate],
  );

  // ------------------------------------------------------------------
  // Agent picker state — localStorage + URL ?picked= round-trip
  // ------------------------------------------------------------------

  // `source_agent_ids` is the DISTINCT set of source agent ids for this
  // campaign, sourced from `campaign_pairs` and stamped onto the
  // single-row campaign DTO by the GET / PATCH handlers. Falling back to
  // an empty list keeps the picker rendering the empty-state card rather
  // than crashing when the field is absent (e.g. older list responses
  // that don't populate it).
  const sourceAgentIds: string[] = useMemo(
    () => campaign.source_agent_ids ?? [],
    [campaign.source_agent_ids],
  );

  const localStorageKey = `meshmon.evaluation.compare.${campaign.id}.agents`;

  const [pickedAgents, setPickedAgents] = useState<Set<string>>(() => {
    // URL param wins on first render.
    if (pickedCandidateCsv) {
      const fromUrl = pickedCandidateCsv.split(",").filter((id) => sourceAgentIds.includes(id));
      if (fromUrl.length > 0) return new Set(fromUrl);
    }
    // Fall back to localStorage.
    try {
      const stored = JSON.parse(localStorage.getItem(localStorageKey) ?? "null") as string[] | null;
      if (Array.isArray(stored)) {
        return new Set(stored.filter((id) => sourceAgentIds.includes(id)));
      }
    } catch {
      // ignore parse errors
    }
    return new Set<string>();
  });

  const handleAgentToggle = useCallback(
    (agentId: string): void => {
      setPickedAgents((prev) => {
        const next = new Set(prev);
        if (next.has(agentId)) {
          next.delete(agentId);
        } else {
          next.add(agentId);
        }
        // Persist to localStorage immediately.
        try {
          localStorage.setItem(localStorageKey, JSON.stringify(Array.from(next)));
        } catch {
          // ignore storage errors
        }
        return next;
      });
    },
    [localStorageKey],
  );

  // Sync URL when pickedAgents changes.
  useEffect(() => {
    const csv = Array.from(pickedAgents).join(",") || undefined;
    setSearchParam({ picked: csv });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- setSearchParam derives from a stable navigate reference; only re-run when pickedAgents actually changes
  }, [pickedAgents, setSearchParam]);

  // ------------------------------------------------------------------
  // Candidate sub-picker state (URL-only, transient)
  // ------------------------------------------------------------------

  const pickedCandidateIps: Set<string> = useMemo(() => {
    if (!candidateCsv) return new Set<string>();
    return new Set(
      candidateCsv
        .split(",")
        .filter((ip) => evaluation.results.candidates.some((c) => c.destination_ip === ip)),
    );
  }, [candidateCsv, evaluation.results.candidates]);

  const handleCandidateToggle = useCallback(
    (ip: string): void => {
      const next = new Set(pickedCandidateIps);
      if (next.has(ip)) {
        next.delete(ip);
      } else {
        next.add(ip);
      }
      setSearchParam({ candidates: Array.from(next).join(",") || undefined });
    },
    [pickedCandidateIps, setSearchParam],
  );

  // ------------------------------------------------------------------
  // Edge-pair data feed (edge_candidate only)
  // ------------------------------------------------------------------

  const edgePairQuery = useEdgePairDetails(isEdgeMode ? campaign.id : undefined, EDGE_PAIR_QUERY);

  const allEdgeRows = useMemo<EvaluationEdgePairDetailDto[]>(
    () => edgePairQuery.data?.pages.flatMap((p) => p.entries) ?? [],
    [edgePairQuery.data],
  );

  // Auto-paginate to get the full dataset.
  useEffect(() => {
    if (edgePairQuery.hasNextPage && !edgePairQuery.isFetchingNextPage) {
      void edgePairQuery.fetchNextPage();
    }
  }, [edgePairQuery.hasNextPage, edgePairQuery.isFetchingNextPage, edgePairQuery.fetchNextPage]);

  // ------------------------------------------------------------------
  // Re-aggregated candidates (edge_candidate only)
  // ------------------------------------------------------------------

  const recomputedCandidates = useMemo<Candidate[]>(() => {
    if (!isEdgeMode) return [];
    const aggregates = aggregateEdgeCandidates(allEdgeRows, pickedAgents);
    return aggregates.map((agg) => {
      const baseline =
        evaluation.results.candidates.find((c) => c.destination_ip === agg.destination_ip) ??
        ({
          destination_ip: agg.destination_ip,
          is_mesh_member: false,
          pairs_improved: 0,
          pairs_total_considered: 0,
        } as Candidate);
      return mergeAggregateIntoCandidate(baseline, agg);
    });
  }, [isEdgeMode, allEdgeRows, pickedAgents, evaluation.results.candidates]);

  // Filter by candidate sub-picker when active.
  const displayedCandidates = useMemo<Candidate[]>(() => {
    if (pickedCandidateIps.size === 0) return recomputedCandidates;
    return recomputedCandidates.filter((c) => pickedCandidateIps.has(c.destination_ip));
  }, [recomputedCandidates, pickedCandidateIps]);

  // ------------------------------------------------------------------
  // Drilldown state
  // ------------------------------------------------------------------

  const [selectedCandidate, setSelectedCandidate] = useState<Candidate | null>(null);

  const handleRowClick = useCallback((candidate: Candidate): void => {
    setSelectedCandidate(candidate);
  }, []);

  const handleCloseDialog = useCallback((): void => {
    setSelectedCandidate(null);
  }, []);

  // ------------------------------------------------------------------
  // Render
  // ------------------------------------------------------------------

  return (
    <div data-testid="compare-view" className="flex flex-col gap-4">
      {/* Storage-filter caveat */}
      <div className="flex items-center gap-2">
        <h2 className="text-sm font-semibold">Compare candidates</h2>
        <TooltipProvider>
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                data-testid="storage-filter-caveat"
                className="text-xs text-muted-foreground underline decoration-dotted cursor-help"
                aria-label={STORAGE_FILTER_CAVEAT}
              >
                ⚠ storage filter note
              </button>
            </TooltipTrigger>
            <TooltipContent className="max-w-xs text-xs">{STORAGE_FILTER_CAVEAT}</TooltipContent>
          </Tooltip>
        </TooltipProvider>
      </div>

      {/* Pick-role radio — diversity/optimization only */}
      {!isEdgeMode ? (
        <PickRoleRadio value={pickRole} onChange={(next) => setSearchParam({ pick_role: next })} />
      ) : null}

      {/* Agent picker */}
      <AgentPicker agentIds={sourceAgentIds} picked={pickedAgents} onToggle={handleAgentToggle} />

      {/* Candidate sub-picker (shown only when at least one agent is picked) */}
      {pickedAgents.size > 0 ? (
        <CandidateSubPicker
          candidates={evaluation.results.candidates}
          pickedIps={pickedCandidateIps}
          onToggle={handleCandidateToggle}
        />
      ) : null}

      {/* Main content area */}
      {isEdgeMode ? (
        <EdgeCompareContent
          query={edgePairQuery}
          candidates={displayedCandidates}
          pickedAgents={pickedAgents}
          onSelectCandidate={handleRowClick}
        />
      ) : (
        <CompareTripleStub />
      )}

      {/* Drilldown dialog */}
      <DrilldownDialog
        candidate={selectedCandidate}
        campaign={campaign}
        evaluation={evaluation}
        onClose={handleCloseDialog}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// PickRoleRadio
// ---------------------------------------------------------------------------

interface PickRoleRadioProps {
  value: PickRole;
  onChange: (next: PickRole) => void;
}

function PickRoleRadio({ value, onChange }: PickRoleRadioProps) {
  return (
    <div
      data-testid="pick-role-radio"
      role="radiogroup"
      aria-label="Filter role"
      className="flex items-center gap-4"
    >
      <span className="text-xs text-muted-foreground font-medium">Filter agents as:</span>
      {(["both", "source", "destination"] as PickRole[]).map((role) => (
        <label
          key={role}
          className="flex items-center gap-1.5 cursor-pointer text-sm"
          data-testid={`pick-role-${role}`}
        >
          <input
            type="radio"
            name="pick_role"
            value={role}
            checked={value === role}
            onChange={() => onChange(role)}
            className="accent-primary"
          />
          <span className="capitalize">{role}</span>
        </label>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// AgentPicker
// ---------------------------------------------------------------------------

interface AgentPickerProps {
  agentIds: string[];
  picked: Set<string>;
  onToggle: (agentId: string) => void;
}

function AgentPicker({ agentIds, picked, onToggle }: AgentPickerProps) {
  if (agentIds.length === 0) {
    return (
      <Card className="p-4 text-sm text-muted-foreground" data-testid="compare-no-agents">
        No source agents configured on this campaign.
      </Card>
    );
  }

  return (
    <Card className="p-4 flex flex-col gap-3">
      <h3 className="text-sm font-semibold">Pick destination agents to compare</h3>
      <div className="flex flex-wrap gap-3">
        {agentIds.map((agentId) => (
          <label key={agentId} className="flex items-center gap-2 cursor-pointer text-sm font-mono">
            <input
              type="checkbox"
              data-testid={`agent-picker-${agentId}`}
              checked={picked.has(agentId)}
              onChange={() => onToggle(agentId)}
              className="accent-primary"
            />
            <span>{agentId}</span>
          </label>
        ))}
      </div>
      {picked.size === 0 ? (
        <p className="text-xs text-muted-foreground">
          Select at least one agent to see re-aggregated candidate metrics.
        </p>
      ) : (
        <p className="text-xs text-muted-foreground">
          {picked.size} of {agentIds.length} agents selected.
        </p>
      )}
    </Card>
  );
}

// ---------------------------------------------------------------------------
// CandidateSubPicker
// ---------------------------------------------------------------------------

interface CandidateSubPickerProps {
  candidates: Candidate[];
  pickedIps: Set<string>;
  onToggle: (ip: string) => void;
}

function CandidateSubPicker({ candidates, pickedIps, onToggle }: CandidateSubPickerProps) {
  const summaryText =
    pickedIps.size === 0
      ? "Restrict to specific candidates"
      : `Filter to specific candidates (${pickedIps.size} of ${candidates.length} selected)`;

  return (
    <details data-testid="candidate-sub-picker-details">
      <summary className="cursor-pointer font-semibold text-sm mb-3 hover:text-foreground transition-colors">
        {summaryText}
      </summary>
      <Card className="p-4 flex flex-col gap-3" data-testid="candidate-sub-picker">
        <div className="flex flex-wrap gap-3">
          {candidates.map((c) => (
            <label
              key={c.destination_ip}
              className="flex items-center gap-2 cursor-pointer text-sm font-mono"
            >
              <input
                type="checkbox"
                data-testid={`candidate-sub-picker-${c.destination_ip}`}
                checked={pickedIps.has(c.destination_ip)}
                onChange={() => onToggle(c.destination_ip)}
                className="accent-primary"
              />
              <CandidateRef
                mode="inline"
                data={{
                  ip: c.destination_ip,
                  display_name: c.display_name,
                  hostname: c.hostname,
                  is_mesh_member: c.is_mesh_member,
                }}
              />
            </label>
          ))}
        </div>
        {pickedIps.size === 0 ? (
          <p className="text-xs text-muted-foreground">All candidates shown.</p>
        ) : (
          <p className="text-xs text-muted-foreground">
            Showing {pickedIps.size} of {candidates.length} candidates.
          </p>
        )}
      </Card>
    </details>
  );
}

// ---------------------------------------------------------------------------
// EdgeCompareContent
// ---------------------------------------------------------------------------

interface EdgeCompareContentProps {
  query: ReturnType<typeof useEdgePairDetails>;
  candidates: Candidate[];
  pickedAgents: Set<string>;
  onSelectCandidate: (candidate: Candidate) => void;
}

function EdgeCompareContent({
  query,
  candidates,
  pickedAgents,
  onSelectCandidate,
}: EdgeCompareContentProps) {
  if (query.isLoading) {
    return (
      <section role="status" aria-live="polite" className="flex flex-col gap-3">
        <span className="sr-only">Loading edge pair data…</span>
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-48 w-full" />
      </section>
    );
  }

  if (query.isError) {
    return (
      <Card className="p-4 text-sm text-destructive" role="alert">
        Failed to load edge pair data: {query.error?.message ?? "unknown error"}
      </Card>
    );
  }

  if (pickedAgents.size === 0) {
    return null;
  }

  if (candidates.length === 0) {
    return (
      <Card className="p-6 text-sm text-muted-foreground" role="status">
        No candidates matched the selected agents.
      </Card>
    );
  }

  return (
    <>
      {query.isFetchingNextPage ? (
        <div className="text-xs text-muted-foreground" role="status">
          Loading further pages…
        </div>
      ) : null}
      <Card className="overflow-hidden" data-testid="compare-candidates-table">
        <Table aria-label="Compare candidates">
          <TableHeader>
            <TableRow>
              <TableHead className="w-10">#</TableHead>
              <TableHead>Candidate</TableHead>
              <TableHead>Coverage</TableHead>
              <TableHead>Mean ping under T</TableHead>
              <TableHead>Route mix</TableHead>
              <TableHead>Coverage-wtd ping</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {candidates.map((candidate, index) => (
              <CompareCandidateRow
                key={candidate.destination_ip}
                candidate={candidate}
                index={index}
                onClick={() => onSelectCandidate(candidate)}
              />
            ))}
          </TableBody>
        </Table>
      </Card>
    </>
  );
}

// ---------------------------------------------------------------------------
// CompareCandidateRow
// ---------------------------------------------------------------------------

interface CompareCandidateRowProps {
  candidate: Candidate;
  index: number;
  onClick: () => void;
}

function formatMs(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  return `${value.toFixed(1)} ms`;
}

function CompareCandidateRow({ candidate, index, onClick }: CompareCandidateRowProps) {
  const refData = {
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

  const direct = candidate.direct_share ?? 0;
  const oneHop = candidate.onehop_share ?? 0;
  const twoHop = candidate.twohop_share ?? 0;

  return (
    <TableRow
      data-testid={`compare-candidate-row-${candidate.destination_ip}`}
      className="cursor-pointer"
      onClick={onClick}
    >
      <TableCell className="text-muted-foreground">{index + 1}</TableCell>
      <TableCell>
        <CandidateRef mode="compact" data={refData} />
      </TableCell>
      <TableCell data-testid={`compare-coverage-${candidate.destination_ip}`}>
        {candidate.coverage_count != null ? (
          <Badge
            variant="secondary"
            className="tabular-nums"
            aria-label={`Coverage: ${candidate.coverage_count}`}
          >
            {candidate.coverage_count}
          </Badge>
        ) : (
          <span className="text-muted-foreground">—</span>
        )}
      </TableCell>
      <TableCell className="text-sm tabular-nums">{formatMs(candidate.mean_ms_under_t)}</TableCell>
      <TableCell>
        <div className="w-24">
          <RouteMixBar direct={direct} oneHop={oneHop} twoHop={twoHop} />
        </div>
      </TableCell>
      <TableCell
        data-testid={`compare-cwp-${candidate.destination_ip}`}
        className="text-sm tabular-nums text-muted-foreground"
      >
        <TooltipProvider>
          <Tooltip>
            <TooltipTrigger asChild>
              <span className="cursor-help">—</span>
            </TooltipTrigger>
            <TooltipContent className="max-w-xs text-xs">{CWP_TOOLTIP}</TooltipContent>
          </Tooltip>
        </TooltipProvider>
      </TableCell>
    </TableRow>
  );
}

// ---------------------------------------------------------------------------
// CompareTripleStub
// ---------------------------------------------------------------------------

function CompareTripleStub() {
  return (
    <Card className="p-6 flex flex-col gap-2" data-testid="compare-triple-stub" role="status">
      <h3 className="text-sm font-semibold">Compare for diversity/optimization</h3>
      <p className="text-sm text-muted-foreground">
        Client-side re-aggregation for diversity and optimization modes is not wired. Per-candidate
        pair_details live behind a paginated endpoint; fetching them per picked candidate is
        deferred. The agent picker and pick-role filter above will drive the query once wired.
      </p>
    </Card>
  );
}
