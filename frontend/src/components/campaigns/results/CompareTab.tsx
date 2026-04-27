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
  type CompareAggregate,
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

type SortKey = "wins" | "coverage" | "mean" | "delta";
type SortDir = "asc" | "desc";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STORAGE_FILTER_CAVEAT =
  "Compare reads the rows actually persisted. If you set tight min_improvement_* storage filters, sub-threshold rows are invisible here.";

const EDGE_PAIR_QUERY: EdgePairsQuery = { limit: 500 };

const WINS_TOOLTIP =
  "Number of picked destination agents (B) where this candidate has the lowest qualifying RTT. Sole qualifiers count as wins; non-qualifying candidates are excluded from the contest for that B.";

const DELTA_TOOLTIP =
  "Average gap (in ms) between this candidate's RTT and the runner-up's across the B's it wins. Negative means this candidate leads. Uncontested wins (only one qualifier) don't contribute.";

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

  // Lookup table from `destination_ip` → baseline candidate. We keep this
  // separate from the aggregate so the comparison-specific columns (wins,
  // delta) stay on `CompareAggregate` without polluting the broader
  // `Candidate` shape.
  const candidatesByIp = useMemo<Map<string, Candidate>>(() => {
    const map = new Map<string, Candidate>();
    for (const c of evaluation.results.candidates) {
      map.set(c.destination_ip, c);
    }
    return map;
  }, [evaluation.results.candidates]);

  const aggregates = useMemo<CompareAggregate[]>(() => {
    if (!isEdgeMode) return [];
    return aggregateEdgeCandidates(allEdgeRows, pickedAgents);
  }, [isEdgeMode, allEdgeRows, pickedAgents]);

  // Filter by candidate sub-picker when active.
  const filteredAggregates = useMemo<CompareAggregate[]>(() => {
    if (pickedCandidateIps.size === 0) return aggregates;
    return aggregates.filter((a) => pickedCandidateIps.has(a.destination_ip));
  }, [aggregates, pickedCandidateIps]);

  // ------------------------------------------------------------------
  // Sort state
  // ------------------------------------------------------------------

  const [sortKey, setSortKey] = useState<SortKey>("wins");
  const [sortDir, setSortDir] = useState<SortDir>("desc");

  const handleSort = useCallback((key: SortKey): void => {
    setSortKey((prevKey) => {
      if (prevKey === key) {
        // Toggle direction when clicking the active column.
        setSortDir((d) => (d === "desc" ? "asc" : "desc"));
        return prevKey;
      }
      // Switching columns picks a sensible default direction: "wins" /
      // "coverage" / "delta" (signed lead) sort high-to-low; "mean" sorts
      // low-to-high (lower latency first).
      setSortDir(key === "mean" ? "asc" : "desc");
      return key;
    });
  }, []);

  const sortedAggregates = useMemo<CompareAggregate[]>(() => {
    const dirSign = sortDir === "asc" ? 1 : -1;
    // Always push nullish metric values to the end regardless of direction
    // so empty / unreachable candidates don't crowd the comparison view.
    const cmpNullable = (a: number | null, b: number | null): number => {
      if (a == null && b == null) return 0;
      if (a == null) return 1;
      if (b == null) return -1;
      return dirSign * (a - b);
    };
    const arr = [...filteredAggregates];
    arr.sort((a, b) => {
      switch (sortKey) {
        case "wins":
          return dirSign * (a.wins - b.wins);
        case "coverage":
          return dirSign * (a.coverage_count - b.coverage_count);
        case "mean":
          return cmpNullable(a.mean_ms_under_t, b.mean_ms_under_t);
        case "delta":
          return cmpNullable(a.avg_delta_to_runner_up_ms, b.avg_delta_to_runner_up_ms);
        default:
          return 0;
      }
    });
    return arr;
  }, [filteredAggregates, sortKey, sortDir]);

  // ------------------------------------------------------------------
  // Drilldown state
  // ------------------------------------------------------------------

  const [selectedCandidate, setSelectedCandidate] = useState<Candidate | null>(null);

  const handleRowClick = useCallback(
    (agg: CompareAggregate): void => {
      const baseline = candidatesByIp.get(agg.destination_ip);
      if (!baseline) {
        // Synthesize a minimal candidate for IPs that exist in edge-pair
        // rows but not in the candidates roster (defensive — shouldn't
        // happen in practice).
        setSelectedCandidate({
          destination_ip: agg.destination_ip,
          is_mesh_member: false,
          pairs_improved: 0,
          pairs_total_considered: 0,
        } as Candidate);
        return;
      }
      setSelectedCandidate(mergeAggregateIntoCandidate(baseline, agg));
    },
    [candidatesByIp],
  );

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
          aggregates={sortedAggregates}
          candidatesByIp={candidatesByIp}
          pickedAgentCount={pickedAgents.size}
          sortKey={sortKey}
          sortDir={sortDir}
          onSort={handleSort}
          onSelectAggregate={handleRowClick}
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
  aggregates: CompareAggregate[];
  candidatesByIp: Map<string, Candidate>;
  pickedAgentCount: number;
  sortKey: SortKey;
  sortDir: SortDir;
  onSort: (key: SortKey) => void;
  onSelectAggregate: (agg: CompareAggregate) => void;
}

function EdgeCompareContent({
  query,
  aggregates,
  candidatesByIp,
  pickedAgentCount,
  sortKey,
  sortDir,
  onSort,
  onSelectAggregate,
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

  if (pickedAgentCount === 0) {
    return null;
  }

  if (aggregates.length === 0) {
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
              <SortableHeader
                label="Wins"
                tooltip={WINS_TOOLTIP}
                active={sortKey === "wins"}
                dir={sortDir}
                onClick={() => onSort("wins")}
                testid="compare-sort-wins"
                ariaSort="Wins per candidate"
              />
              <SortableHeader
                label="Δ vs runner-up"
                tooltip={DELTA_TOOLTIP}
                active={sortKey === "delta"}
                dir={sortDir}
                onClick={() => onSort("delta")}
                testid="compare-sort-delta"
                ariaSort="Average lead over runner-up"
              />
              <SortableHeader
                label="Coverage"
                active={sortKey === "coverage"}
                dir={sortDir}
                onClick={() => onSort("coverage")}
                testid="compare-sort-coverage"
                ariaSort="Coverage count"
              />
              <SortableHeader
                label="Mean ping under T"
                active={sortKey === "mean"}
                dir={sortDir}
                onClick={() => onSort("mean")}
                testid="compare-sort-mean"
                ariaSort="Mean RTT for qualifying destinations"
              />
              <TableHead>Route mix</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {aggregates.map((agg, index) => (
              <CompareCandidateRow
                key={agg.destination_ip}
                aggregate={agg}
                candidate={candidatesByIp.get(agg.destination_ip)}
                index={index}
                isTopRanked={
                  index === 0 && sortKey === "wins" && sortDir === "desc" && agg.wins > 0
                }
                onClick={() => onSelectAggregate(agg)}
              />
            ))}
          </TableBody>
        </Table>
      </Card>
    </>
  );
}

// ---------------------------------------------------------------------------
// SortableHeader
// ---------------------------------------------------------------------------

interface SortableHeaderProps {
  label: string;
  tooltip?: string;
  active: boolean;
  dir: SortDir;
  onClick: () => void;
  testid: string;
  ariaSort: string;
}

function SortableHeader({
  label,
  tooltip,
  active,
  dir,
  onClick,
  testid,
  ariaSort,
}: SortableHeaderProps) {
  const arrow = active ? (dir === "desc" ? "↓" : "↑") : "";
  const button = (
    <button
      type="button"
      onClick={onClick}
      data-testid={testid}
      aria-label={`Sort by ${ariaSort}, currently ${active ? dir : "inactive"}`}
      className={
        active
          ? "inline-flex items-center gap-1 text-foreground font-semibold"
          : "inline-flex items-center gap-1 text-muted-foreground hover:text-foreground"
      }
    >
      <span>{label}</span>
      <span className="text-xs">{arrow || "↕"}</span>
    </button>
  );
  if (!tooltip) {
    return <TableHead>{button}</TableHead>;
  }
  return (
    <TableHead>
      <TooltipProvider>
        <Tooltip>
          <TooltipTrigger asChild>{button}</TooltipTrigger>
          <TooltipContent className="max-w-xs text-xs">{tooltip}</TooltipContent>
        </Tooltip>
      </TooltipProvider>
    </TableHead>
  );
}

// ---------------------------------------------------------------------------
// CompareCandidateRow
// ---------------------------------------------------------------------------

interface CompareCandidateRowProps {
  aggregate: CompareAggregate;
  candidate: Candidate | undefined;
  index: number;
  isTopRanked: boolean;
  onClick: () => void;
}

function formatMs(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  return `${value.toFixed(1)} ms`;
}

function formatDelta(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  // Negative = lead. Render with explicit sign so the direction is obvious.
  const sign = value < 0 ? "−" : "+";
  return `${sign}${Math.abs(value).toFixed(1)} ms`;
}

function CompareCandidateRow({
  aggregate,
  candidate,
  index,
  isTopRanked,
  onClick,
}: CompareCandidateRowProps) {
  const refData = candidate
    ? {
        ip: candidate.destination_ip,
        display_name: candidate.display_name,
        city: candidate.city,
        country_code: candidate.country_code,
        asn: candidate.asn,
        network_operator: candidate.network_operator,
        hostname: candidate.hostname,
        is_mesh_member: candidate.is_mesh_member,
        agent_id: (candidate as Candidate & { agent_id?: string | null }).agent_id,
      }
    : { ip: aggregate.destination_ip, is_mesh_member: false };

  const direct = aggregate.direct_share ?? 0;
  const oneHop = aggregate.onehop_share ?? 0;
  const twoHop = aggregate.twohop_share ?? 0;

  const delta = aggregate.avg_delta_to_runner_up_ms;
  const deltaClass =
    delta == null
      ? "text-muted-foreground"
      : delta < 0
        ? "text-emerald-600 dark:text-emerald-400"
        : "text-amber-600 dark:text-amber-400";

  return (
    <TableRow
      data-testid={`compare-candidate-row-${aggregate.destination_ip}`}
      className="cursor-pointer"
      onClick={onClick}
    >
      <TableCell className="text-muted-foreground">
        <div className="flex items-center gap-1">
          <span>{index + 1}</span>
          {isTopRanked ? (
            <Badge
              variant="secondary"
              className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
              data-testid={`compare-top-${aggregate.destination_ip}`}
            >
              Top
            </Badge>
          ) : null}
        </div>
      </TableCell>
      <TableCell>
        <CandidateRef mode="compact" data={refData} />
      </TableCell>
      <TableCell
        data-testid={`compare-wins-${aggregate.destination_ip}`}
        className="text-sm tabular-nums"
      >
        <span className="font-semibold">{aggregate.wins}</span>
        <span className="text-muted-foreground"> / {aggregate.total_picked}</span>
      </TableCell>
      <TableCell
        data-testid={`compare-delta-${aggregate.destination_ip}`}
        className={`text-sm tabular-nums ${deltaClass}`}
      >
        {formatDelta(delta)}
      </TableCell>
      <TableCell data-testid={`compare-coverage-${aggregate.destination_ip}`}>
        <Badge
          variant="secondary"
          className="tabular-nums"
          aria-label={`Coverage: ${aggregate.coverage_count}`}
        >
          {aggregate.coverage_count}
        </Badge>
      </TableCell>
      <TableCell className="text-sm tabular-nums">{formatMs(aggregate.mean_ms_under_t)}</TableCell>
      <TableCell>
        <div className="w-24">
          <RouteMixBar direct={direct} oneHop={oneHop} twoHop={twoHop} />
        </div>
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
