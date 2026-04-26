/**
 * HeatmapTab — (X candidate) × (B destination agent) latency matrix.
 *
 * Only rendered for `evaluation_mode === "edge_candidate"`. Rows represent
 * B (unique destination_agent_ids), columns represent X (unique candidate_ips).
 * Each cell shows the best_route_ms integer or "—" for unreachable pairs.
 *
 * ## Color-tier model (K-1, 4-handle)
 *
 * Five color tiers are derived from four boundary values `[b0, b1, b2, b3]`:
 * - Tier 1 (excellent): ms < b0
 * - Tier 2 (good):      b0 ≤ ms < b1
 * - Tier 3 (fair):      b1 ≤ ms < b2
 * - Tier 4 (marginal):  b2 ≤ ms < b3
 * - Tier 5 (poor):      ms ≥ b3
 *
 * Default boundaries are computed from `useful_latency_ms` (T): `[0.4·T, T, 2·T, 4·T]`.
 * Fallback when T is absent: `[32, 80, 160, 320]` ms.
 *
 * ## Tier boundary customization
 *
 * The `HeatmapColorEditor` popover exposes four drag handles, one per
 * boundary. Changes are written to localStorage at
 * `meshmon.evaluation.heatmap.edge_candidate.colors` (a JSON array of four
 * numbers) and restored on mount. The key is per-mode so a future mode with
 * its own heatmap can store independent boundaries.
 *
 * ## Sort state
 *
 * Rows and columns are independently sortable via the column and row header
 * controls. Sort state round-trips through four URL search params:
 * - `hm_row_sort` — row sort key (`destination_agent_id` | `mean_ms` | `destinations_qualifying`)
 * - `hm_row_dir`  — row sort direction (`asc` | `desc`)
 * - `hm_col_sort` — column sort key (`candidate_ip` | `coverage_weighted_ping_ms` | `coverage_count`)
 * - `hm_col_dir`  — column sort direction (`asc` | `desc`)
 *
 * ## Interaction
 *
 * Clicking a cell opens the `DrilldownDialog` scoped to `(X, B)`, showing
 * the winning route's legs and per-leg substitution flags.
 *
 * ## Virtualization
 *
 * `@tanstack/react-virtual` kicks in when either axis exceeds 30 items,
 * keeping the DOM bounded regardless of fleet size.
 */

import { useVirtualizer } from "@tanstack/react-virtual";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Campaign } from "@/api/hooks/campaigns";
import type { Evaluation, EvaluationEdgePairDetailDto } from "@/api/hooks/evaluation";
import { useEdgePairDetails } from "@/api/hooks/evaluation";
import type { components } from "@/api/schema.gen";
import { DrilldownDialog } from "@/components/campaigns/results/DrilldownDialog";
import { HeatmapColorEditor } from "@/components/campaigns/results/HeatmapColorEditor";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import type { CampaignDetailSearch } from "@/router/index";

type EvaluationCandidateDto = components["schemas"]["EvaluationCandidateDto"];

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/** Virtualization threshold — enable when either dimension exceeds this. */
const VIRTUALIZE_THRESHOLD = 30;

/** Height of each data row in the virtualized grid. */
const ROW_HEIGHT = 40;
/** Width of each column cell in the virtualized grid. */
const COL_WIDTH = 80;
/** Width of the sticky row-header column. */
const ROW_HEADER_WIDTH = 180;
/** Height of the sticky column-header row. */
const COL_HEADER_HEIGHT = 52;

/** Pre-measured initial rect so jsdom tests see rows without layout. */
const INITIAL_SCROLL_RECT = { width: 1200, height: 600 };

// ---------------------------------------------------------------------------
// Tier helpers
// ---------------------------------------------------------------------------

/** Tier index 1–5 for a reachable pair, or 0 for unreachable. */
export function getTier(ms: number, boundaries: number[]): 1 | 2 | 3 | 4 | 5 {
  if (ms < boundaries[0]!) return 1;
  if (ms < boundaries[1]!) return 2;
  if (ms < boundaries[2]!) return 3;
  if (ms < boundaries[3]!) return 4;
  return 5;
}

interface TierStyle {
  background: string;
  color: string;
}

const TIER_STYLES: Record<number, TierStyle> = {
  1: { background: "var(--hm-tier-1)", color: "#fff" },
  2: { background: "var(--hm-tier-2)", color: "#fff" },
  3: { background: "var(--hm-tier-3)", color: "var(--hm-tier-3-text)" },
  4: { background: "var(--hm-tier-4)", color: "var(--hm-tier-4-text)" },
  5: { background: "var(--hm-tier-5)", color: "#fff" },
  0: { background: "var(--hm-tier-x)", color: "#fff" }, // unreachable
};

function tierStyle(ms: number | null, boundaries: number[]): TierStyle {
  if (ms === null) return TIER_STYLES[0]!;
  return TIER_STYLES[getTier(ms, boundaries)]!;
}

// ---------------------------------------------------------------------------
// Sort helpers
// ---------------------------------------------------------------------------

export type RowSortKey = "destination_agent_id" | "mean_ms" | "destinations_qualifying";
export type ColSortKey = "candidate_ip" | "coverage_weighted_ping_ms" | "coverage_count";
export type SortDir = "asc" | "desc";

// ---------------------------------------------------------------------------
// Pivot helpers
// ---------------------------------------------------------------------------

/** Lookup map: `${candidate_ip}::${destination_agent_id}` → row */
type CellMap = Map<string, EvaluationEdgePairDetailDto>;

interface PivotResult {
  candidateIps: string[];
  agentIds: string[];
  cellMap: CellMap;
}

function pivotRows(rows: EvaluationEdgePairDetailDto[]): PivotResult {
  const candidateSet = new Set<string>();
  const agentSet = new Set<string>();
  const cellMap: CellMap = new Map();

  for (const row of rows) {
    candidateSet.add(row.candidate_ip);
    agentSet.add(row.destination_agent_id);
    cellMap.set(`${row.candidate_ip}::${row.destination_agent_id}`, row);
  }

  return {
    candidateIps: Array.from(candidateSet),
    agentIds: Array.from(agentSet),
    cellMap,
  };
}

/** Mean best_route_ms across all candidates for one agent row (null = no data). */
function rowMean(agentId: string, candidateIps: string[], cellMap: CellMap): number | null {
  let sum = 0;
  let count = 0;
  for (const ip of candidateIps) {
    const row = cellMap.get(`${ip}::${agentId}`);
    // `best_route_ms` is `null` for unreachable rows.
    if (row && !row.is_unreachable && row.best_route_ms != null) {
      sum += row.best_route_ms;
      count++;
    }
  }
  return count > 0 ? sum / count : null;
}

/** Count of qualifying (qualifies_under_t) entries for one agent row. */
function rowQualifying(agentId: string, candidateIps: string[], cellMap: CellMap): number {
  let count = 0;
  for (const ip of candidateIps) {
    const row = cellMap.get(`${ip}::${agentId}`);
    if (row?.qualifies_under_t) count++;
  }
  return count;
}

/** Coverage count (non-unreachable rows) for one candidate column. */
function colCoverage(ip: string, agentIds: string[], cellMap: CellMap): number {
  let count = 0;
  for (const id of agentIds) {
    const row = cellMap.get(`${ip}::${id}`);
    if (row && !row.is_unreachable) count++;
  }
  return count;
}

/** Coverage-weighted ping for one candidate column. */
function colWeightedPing(ip: string, agentIds: string[], cellMap: CellMap): number | null {
  let sum = 0;
  let count = 0;
  for (const id of agentIds) {
    const row = cellMap.get(`${ip}::${id}`);
    // `best_route_ms` is `null` for unreachable rows.
    if (row && !row.is_unreachable && row.best_route_ms != null) {
      sum += row.best_route_ms;
      count++;
    }
  }
  return count > 0 ? sum / count : null;
}

function sortedAgentIds(
  agentIds: string[],
  candidateIps: string[],
  cellMap: CellMap,
  sortKey: RowSortKey,
  dir: SortDir,
): string[] {
  const mult = dir === "asc" ? 1 : -1;
  return [...agentIds].sort((a, b) => {
    if (sortKey === "destination_agent_id") {
      return mult * a.localeCompare(b);
    }
    if (sortKey === "mean_ms") {
      const ma = rowMean(a, candidateIps, cellMap) ?? Infinity;
      const mb = rowMean(b, candidateIps, cellMap) ?? Infinity;
      return mult * (ma - mb);
    }
    // destinations_qualifying
    const qa = rowQualifying(a, candidateIps, cellMap);
    const qb = rowQualifying(b, candidateIps, cellMap);
    return mult * (qa - qb);
  });
}

function sortedCandidateIps(
  candidateIps: string[],
  agentIds: string[],
  cellMap: CellMap,
  sortKey: ColSortKey,
  dir: SortDir,
): string[] {
  const mult = dir === "asc" ? 1 : -1;
  return [...candidateIps].sort((a, b) => {
    if (sortKey === "candidate_ip") {
      return mult * a.localeCompare(b);
    }
    if (sortKey === "coverage_count") {
      const ca = colCoverage(a, agentIds, cellMap);
      const cb = colCoverage(b, agentIds, cellMap);
      return mult * (ca - cb);
    }
    // coverage_weighted_ping_ms
    const pa = colWeightedPing(a, agentIds, cellMap) ?? Infinity;
    const pb = colWeightedPing(b, agentIds, cellMap) ?? Infinity;
    return mult * (pa - pb);
  });
}

// ---------------------------------------------------------------------------
// Boundaries helper
// ---------------------------------------------------------------------------

export function readBoundaries(
  mode: string,
  usefulLatencyMs: number | null | undefined,
): number[] {
  const T = usefulLatencyMs ?? 80;
  const defaultBoundaries = [0.4 * T, T, 2 * T, 4 * T];
  try {
    const stored = JSON.parse(
      localStorage.getItem(`meshmon.evaluation.heatmap.${mode}.colors`) ?? "null",
    ) as number[] | null;
    if (Array.isArray(stored) && stored.length === 4) return stored;
  } catch {
    // ignore parse errors
  }
  return defaultBoundaries;
}

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface HeatmapTabProps {
  campaign: Campaign;
  evaluation: Evaluation;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function HeatmapTab({ campaign, evaluation }: HeatmapTabProps) {
  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as CampaignDetailSearch;

  // Sort state from URL params (with defaults).
  const rowSortKey = search.hm_row_sort ?? "destination_agent_id";
  const rowSortDir = search.hm_row_dir ?? "asc";
  const colSortKey = search.hm_col_sort ?? "coverage_weighted_ping_ms";
  const colSortDir = search.hm_col_dir ?? "asc";

  const [colorEditorOpen, setColorEditorOpen] = useState(false);
  // Revision counter incremented when the editor saves so the heatmap re-reads boundaries.
  const [boundaryRevision, setBoundaryRevision] = useState(0);

  // Selected cell for drilldown
  const [selectedCell, setSelectedCell] = useState<{
    candidateIp: string;
    agentId: string;
  } | null>(null);

  const setSort = useCallback(
    (updates: Partial<{
      hm_row_sort: RowSortKey;
      hm_row_dir: SortDir;
      hm_col_sort: ColSortKey;
      hm_col_dir: SortDir;
    }>): void => {
      // useNavigate() is untyped in non-strict mode; cast through unknown so
      // TypeScript doesn't try to match the global router union.
      const nav = navigate as unknown as (opts: {
        search: (prev: Record<string, unknown>) => Record<string, unknown>;
        replace: boolean;
      }) => void;
      nav({
        search: (prev) => ({ ...prev, ...updates }),
        replace: true,
      });
    },
    [navigate],
  );

  // Fetch ALL edge pair rows (no filter). The heatmap needs the full matrix.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const emptyQuery = useMemo(() => ({ limit: 500 }), []);
  const query = useEdgePairDetails(campaign.id, emptyQuery);

  const allRows = useMemo<EvaluationEdgePairDetailDto[]>(
    () => query.data?.pages.flatMap((p) => p.entries) ?? [],
    [query.data],
  );

  // Auto-fetch remaining pages so the heatmap always shows the full matrix.
  useEffect(() => {
    if (query.hasNextPage && !query.isFetchingNextPage) {
      void query.fetchNextPage();
    }
  }, [query.hasNextPage, query.isFetchingNextPage, query.fetchNextPage]);

  const boundaries = useMemo(
    () => readBoundaries(evaluation.evaluation_mode, evaluation.useful_latency_ms),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [evaluation.evaluation_mode, evaluation.useful_latency_ms, boundaryRevision],
  );

  const { candidateIps: rawCandidateIps, agentIds: rawAgentIds, cellMap } = useMemo(
    () => pivotRows(allRows),
    [allRows],
  );

  const orderedAgentIds = useMemo(
    () => sortedAgentIds(rawAgentIds, rawCandidateIps, cellMap, rowSortKey, rowSortDir),
    [rawAgentIds, rawCandidateIps, cellMap, rowSortKey, rowSortDir],
  );

  const orderedCandidateIps = useMemo(
    () => sortedCandidateIps(rawCandidateIps, rawAgentIds, cellMap, colSortKey, colSortDir),
    [rawCandidateIps, rawAgentIds, cellMap, colSortKey, colSortDir],
  );

  // Drilldown candidate lookup
  const selectedCandidate: EvaluationCandidateDto | null = useMemo(() => {
    if (!selectedCell) return null;
    return (
      evaluation.results.candidates.find(
        (c) => c.destination_ip === selectedCell.candidateIp,
      ) ?? null
    );
  }, [selectedCell, evaluation.results.candidates]);

  // Virtualization refs and virtualizers
  const scrollRef = useRef<HTMLDivElement>(null);

  const useRowVirt = orderedAgentIds.length > VIRTUALIZE_THRESHOLD;
  const useColVirt = orderedCandidateIps.length > VIRTUALIZE_THRESHOLD;

  const rowVirtualizer = useVirtualizer({
    count: orderedAgentIds.length,
    getScrollElement: () => (useRowVirt ? scrollRef.current : null),
    estimateSize: () => ROW_HEIGHT,
    overscan: 5,
    initialRect: INITIAL_SCROLL_RECT,
  });

  const colVirtualizer = useVirtualizer({
    count: orderedCandidateIps.length,
    getScrollElement: () => (useColVirt ? scrollRef.current : null),
    estimateSize: () => COL_WIDTH,
    overscan: 5,
    horizontal: true,
    initialRect: INITIAL_SCROLL_RECT,
  });

  // ---------------------------------------------------------------------------
  // Loading / error
  // ---------------------------------------------------------------------------

  if (query.isLoading) {
    return (
      <section
        data-testid="heatmap-tab"
        role="status"
        aria-live="polite"
        className="flex flex-col gap-3"
      >
        <span className="sr-only">Loading heatmap…</span>
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-64 w-full" />
      </section>
    );
  }

  if (query.isError) {
    return (
      <section data-testid="heatmap-tab" className="flex flex-col gap-3">
        <Card className="p-4 text-sm text-destructive" role="alert">
          Failed to load heatmap data: {query.error?.message ?? "unknown error"}
        </Card>
      </section>
    );
  }

  if (allRows.length === 0) {
    return (
      <section data-testid="heatmap-tab" className="flex flex-col gap-3">
        <Card className="p-6 text-sm text-muted-foreground" role="status">
          No edge pair data for this campaign yet.
        </Card>
      </section>
    );
  }

  // ---------------------------------------------------------------------------
  // Render helpers
  // ---------------------------------------------------------------------------

  const virtualRows = useRowVirt ? rowVirtualizer.getVirtualItems() : null;
  const virtualCols = useColVirt ? colVirtualizer.getVirtualItems() : null;

  // Virtual items or identity mapping for non-virtualized dimensions
  const rowItems =
    virtualRows ??
    orderedAgentIds.map((id, index) => ({
      index,
      key: id,
      start: index * ROW_HEIGHT,
      size: ROW_HEIGHT,
    }));
  const colItems =
    virtualCols ??
    orderedCandidateIps.map((ip, index) => ({
      index,
      key: ip,
      start: index * COL_WIDTH,
      size: COL_WIDTH,
    }));

  const innerRowHeight = useRowVirt ? rowVirtualizer.getTotalSize() : undefined;
  const innerColWidth = useColVirt ? colVirtualizer.getTotalSize() : undefined;
  const totalWidth = ROW_HEADER_WIDTH + (innerColWidth ?? orderedCandidateIps.length * COL_WIDTH);

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  return (
    <section data-testid="heatmap-tab" className="flex flex-col gap-4">
      {/* Toolbar */}
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-sm font-semibold" data-testid="heatmap-dimension-label">
          {orderedCandidateIps.length} candidates × {orderedAgentIds.length} agents
        </span>
        <div className="ml-auto flex items-center gap-2 flex-wrap">
          {/* Row sort */}
          <AxisSortButton
            label="Rows"
            options={ROW_SORT_OPTIONS}
            activeKey={rowSortKey}
            activeDir={rowSortDir}
            onSort={(key, dir) =>
              setSort({ hm_row_sort: key as RowSortKey, hm_row_dir: dir })
            }
          />
          {/* Col sort */}
          <AxisSortButton
            label="Cols"
            options={COL_SORT_OPTIONS}
            activeKey={colSortKey}
            activeDir={colSortDir}
            onSort={(key, dir) =>
              setSort({ hm_col_sort: key as ColSortKey, hm_col_dir: dir })
            }
          />
          {/* Color editor */}
          <HeatmapColorEditor
            open={colorEditorOpen}
            onOpenChange={setColorEditorOpen}
            mode={evaluation.evaluation_mode}
            usefulLatencyMs={evaluation.useful_latency_ms ?? null}
            onSaved={() => setBoundaryRevision((r) => r + 1)}
          />
        </div>
      </div>

      {/* Loading indicator during multi-page fetch */}
      {query.isFetchingNextPage && (
        <div className="text-xs text-muted-foreground" role="status">
          Loading further pages…
        </div>
      )}

      {/* Heatmap grid */}
      <Card className="overflow-hidden">
        <div
          ref={scrollRef}
          className="overflow-auto"
          style={{ maxHeight: "70vh" }}
          data-testid="heatmap-scroll"
        >
          <div style={{ minWidth: totalWidth, position: "relative" }}>
            {/* Column headers */}
            <div
              className="sticky top-0 z-20 flex bg-muted/80 backdrop-blur-sm border-b"
              style={{ height: COL_HEADER_HEIGHT }}
              data-testid="heatmap-col-headers"
            >
              {/* Top-left corner cell */}
              <div
                className="shrink-0 border-r flex items-end px-2 pb-1 text-xs text-muted-foreground font-medium"
                style={{ width: ROW_HEADER_WIDTH, minWidth: ROW_HEADER_WIDTH }}
              >
                B ↓ / X →
              </div>
              {/* Column header cells */}
              <div
                className="relative flex-1"
                style={innerColWidth ? { width: innerColWidth } : undefined}
              >
                {colItems.map((vc) => {
                  const ip = orderedCandidateIps[vc.index]!;
                  return (
                    <div
                      key={vc.key}
                      data-testid={`heatmap-col-header-${ip}`}
                      className="absolute top-0 flex items-end justify-center pb-1 text-xs font-mono overflow-hidden"
                      style={{
                        left: vc.start,
                        width: vc.size,
                        height: COL_HEADER_HEIGHT,
                      }}
                      title={ip}
                    >
                      <span className="truncate max-w-full px-1">{ip}</span>
                    </div>
                  );
                })}
              </div>
            </div>

            {/* Rows */}
            <div
              className="relative"
              style={innerRowHeight ? { height: innerRowHeight } : undefined}
            >
              {rowItems.map((vr) => {
                const agentId = orderedAgentIds[vr.index]!;
                return (
                  <div
                    key={vr.key}
                    data-testid={`heatmap-row-${agentId}`}
                    className="absolute left-0 right-0 flex border-b last:border-0"
                    style={{ top: vr.start, height: vr.size }}
                  >
                    {/* Row header */}
                    <div
                      className="sticky left-0 z-10 shrink-0 flex items-center bg-background border-r px-2 text-xs font-mono truncate"
                      style={{ width: ROW_HEADER_WIDTH, minWidth: ROW_HEADER_WIDTH }}
                      title={agentId}
                    >
                      {agentId}
                    </div>

                    {/* Cells */}
                    <div
                      className="relative flex-1"
                      style={innerColWidth ? { width: innerColWidth } : undefined}
                    >
                      {colItems.map((vc) => {
                        const ip = orderedCandidateIps[vc.index]!;
                        const row = cellMap.get(`${ip}::${agentId}`);
                        // `best_route_ms` is `null` for unreachable rows.
                        const ms =
                          row && !row.is_unreachable && row.best_route_ms != null
                            ? row.best_route_ms
                            : null;
                        const style = tierStyle(ms, boundaries);

                        return (
                          <button
                            type="button"
                            key={vc.key}
                            data-testid={`heatmap-cell-${ip}-${agentId}`}
                            className="absolute flex items-center justify-center text-xs tabular-nums font-mono cursor-pointer hover:ring-2 hover:ring-inset hover:ring-primary/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary"
                            style={{
                              left: vc.start,
                              width: vc.size,
                              height: "100%",
                              background: style.background,
                              color: style.color,
                            }}
                            onClick={() =>
                              setSelectedCell({ candidateIp: ip, agentId })
                            }
                            aria-label={`${ip} × ${agentId}: ${ms !== null ? `${Math.round(ms)} ms` : "unreachable"}`}
                          >
                            {ms !== null ? Math.round(ms) : "—"}
                          </button>
                        );
                      })}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        </div>
      </Card>

      {/* Drilldown dialog */}
      <DrilldownDialog
        candidate={selectedCandidate}
        campaign={campaign}
        evaluation={evaluation}
        onClose={() => setSelectedCell(null)}
      />
    </section>
  );
}

// ---------------------------------------------------------------------------
// Sort button helpers
// ---------------------------------------------------------------------------

const ROW_SORT_OPTIONS: { key: RowSortKey; label: string }[] = [
  { key: "destination_agent_id", label: "Agent ID" },
  { key: "mean_ms", label: "Mean RTT" },
  { key: "destinations_qualifying", label: "Qualifying" },
];

const COL_SORT_OPTIONS: { key: ColSortKey; label: string }[] = [
  { key: "candidate_ip", label: "Candidate IP" },
  { key: "coverage_weighted_ping_ms", label: "Weighted RTT" },
  { key: "coverage_count", label: "Coverage" },
];

interface AxisSortButtonProps {
  label: string;
  options: { key: string; label: string }[];
  activeKey: string;
  activeDir: SortDir;
  onSort: (key: string, dir: SortDir) => void;
}

const SORT_DEFAULT_DIR: Record<string, SortDir> = {
  mean_ms: "asc",
  coverage_weighted_ping_ms: "asc",
  destinations_qualifying: "desc",
  coverage_count: "desc",
  destination_agent_id: "asc",
  candidate_ip: "asc",
};

function AxisSortButton({
  label,
  options,
  activeKey,
  activeDir,
  onSort,
}: AxisSortButtonProps) {
  return (
    <div className="flex items-center gap-1">
      <span className="text-xs text-muted-foreground">{label}:</span>
      {options.map(({ key, label: optLabel }) => {
        const isActive = key === activeKey;
        const nextDir: SortDir = isActive
          ? activeDir === "asc" ? "desc" : "asc"
          : SORT_DEFAULT_DIR[key] ?? "asc";
        return (
          <Button
            key={key}
            type="button"
            size="sm"
            variant={isActive ? "default" : "outline"}
            className="h-7 px-2 text-xs"
            onClick={() => onSort(key, nextDir)}
            aria-pressed={isActive}
          >
            {optLabel}
            {isActive ? (activeDir === "asc" ? " ▲" : " ▼") : ""}
          </Button>
        );
      })}
    </div>
  );
}
