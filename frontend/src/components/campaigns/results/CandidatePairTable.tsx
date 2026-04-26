/**
 * Virtualized pair-detail table for the drilldown dialog.
 *
 * Mirrors the recipe in `RawTab.tsx`: CSS-grid rows, sticky header
 * above the scroll container, `useVirtualizer` with overscan and an
 * `INITIAL_SCROLL_RECT` so jsdom mounts produce non-zero dimensions.
 *
 * Sign convention: `improvement_ms > 0` means the A→X→B transit is
 * faster than the direct A→B baseline. Positive values render green;
 * negative red. Δ% is defensive — non-finite or `direct_rtt_ms ≤ 0`
 * collapses to `—` so a single corrupted row never crashes the
 * virtualized render. The backend's I3 NaN/Infinity guard already
 * filters these rows out of the response, so this is belt-and-braces.
 */

import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef } from "react";
import type { AgentSummary } from "@/api/hooks/agents";
import type { EvaluationPairDetail, PairDetailSortCol } from "@/api/hooks/evaluation-pairs";
import { IpHostname } from "@/components/ip-hostname";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { SortableHeader, type SortState } from "@/components/ui/sortable-header";
import { formatLossRatio } from "@/lib/format-loss";
import { cn } from "@/lib/utils";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/** Row height estimate in px — keep in sync with the row class height. */
const ROW_HEIGHT_ESTIMATE = 40;

/** Max height of the virtualized scroll container. */
const SCROLL_MAX_HEIGHT = "55vh";

/**
 * Initial viewport rect fed to the virtualizer before the ResizeObserver
 * fires. jsdom never measures real layout; a generous default keeps the
 * test renderer populated while the real observer supersedes it within a
 * frame in production.
 */
const INITIAL_SCROLL_RECT = { width: 1024, height: 600 };

/** Grid track template — fixed widths for dense columns, `fr` for wide text. */
const GRID_TEMPLATE =
  "minmax(140px, 1fr) minmax(140px, 1fr) 110px 80px 110px 80px 90px 90px 90px 96px";

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface CandidatePairTableProps {
  rows: EvaluationPairDetail[];
  agentsById: Map<string, AgentSummary>;
  sort: SortState<PairDetailSortCol>;
  onSortChange: (col: PairDetailSortCol | null, dir: "asc" | "desc" | null) => void;
  hasNextPage: boolean;
  isFetchingNextPage: boolean;
  fetchNextPage: () => void;
  onOpenMtr: (measurementId: number, label: string) => void;
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

function formatMs(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return "—";
  return `${value.toFixed(1)} ms`;
}

function formatImprovement(value: number): string {
  if (!Number.isFinite(value)) return "—";
  const rounded = Math.round(value * 10) / 10;
  const sign = rounded > 0 ? "+" : "";
  return `${sign}${rounded.toFixed(1)} ms`;
}

function improvementClass(value: number): string {
  if (!Number.isFinite(value)) return "text-muted-foreground";
  if (value > 0) return "text-emerald-600 dark:text-emerald-400 font-medium";
  if (value < 0) return "text-destructive font-medium";
  return "text-muted-foreground";
}

/**
 * Defensive Δ% formatter. A non-finite improvement, a non-finite or
 * non-positive direct RTT, or a non-finite ratio collapses to `—` so
 * a single corrupted row never crashes the virtualized render.
 */
function formatDeltaPercent(improvementMs: number, directRttMs: number): string {
  if (!Number.isFinite(improvementMs) || !Number.isFinite(directRttMs) || directRttMs <= 0) {
    return "—";
  }
  const ratio = improvementMs / directRttMs;
  if (!Number.isFinite(ratio)) return "—";
  const pct = Math.round(ratio * 1000) / 10;
  const sign = pct > 0 ? "+" : "";
  return `${sign}${pct.toFixed(1)} %`;
}

function agentLabel(agent: AgentSummary | undefined, fallback: string): string {
  if (!agent) return fallback;
  return agent.display_name || agent.id;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function CandidatePairTable({
  rows,
  agentsById,
  sort,
  onSortChange,
  hasNextPage,
  isFetchingNextPage,
  fetchNextPage,
  onOpenMtr,
}: CandidatePairTableProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT_ESTIMATE,
    overscan: 10,
    initialRect: INITIAL_SCROLL_RECT,
  });
  const virtualItems = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();

  // Auto-fetch when the last rendered virtual row approaches the tail
  // of the loaded set. Cheap no-op when `hasNextPage` is false.
  useEffect(() => {
    if (!hasNextPage || isFetchingNextPage) return;
    const lastItem = virtualItems[virtualItems.length - 1];
    if (!lastItem) return;
    if (lastItem.index >= rows.length - 5) {
      fetchNextPage();
    }
  }, [virtualItems, rows.length, hasNextPage, isFetchingNextPage, fetchNextPage]);

  return (
    // biome-ignore lint/a11y/useSemanticElements: virtualized row/grid rendering requires CSS grid on every row; switching to <table> forces `table-layout: fixed` which breaks the `fr` tracks — same rationale as RawTab/CatalogueTable.
    <div className="rounded-md border" role="table" aria-label="Pair details">
      {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale above. */}
      {/* biome-ignore lint/a11y/useFocusableInteractive: role="row" is a grouping role in the ARIA table pattern — not an interactive control. */}
      <div
        role="row"
        className="grid w-full border-b bg-muted/30 px-3 py-2 text-xs font-medium uppercase tracking-wide text-muted-foreground"
        style={{ gridTemplateColumns: GRID_TEMPLATE }}
      >
        <HeaderCell aria-sort={ariaSortFor(sort, "source_agent_id")}>
          <SortableHeader
            col="source_agent_id"
            label="Source"
            sort={sort}
            onSortChange={onSortChange}
          />
        </HeaderCell>
        <HeaderCell aria-sort={ariaSortFor(sort, "destination_agent_id")}>
          <SortableHeader
            col="destination_agent_id"
            label="Destination"
            sort={sort}
            onSortChange={onSortChange}
          />
        </HeaderCell>
        <HeaderCell className="text-right" aria-sort={ariaSortFor(sort, "direct_rtt_ms")}>
          <SortableHeader
            col="direct_rtt_ms"
            label="Direct RTT"
            sort={sort}
            onSortChange={onSortChange}
          />
        </HeaderCell>
        <HeaderCell className="text-right" aria-sort={ariaSortFor(sort, "direct_loss_ratio")}>
          <SortableHeader
            col="direct_loss_ratio"
            label="Direct loss"
            sort={sort}
            onSortChange={onSortChange}
          />
        </HeaderCell>
        <HeaderCell className="text-right" aria-sort={ariaSortFor(sort, "transit_rtt_ms")}>
          <SortableHeader
            col="transit_rtt_ms"
            label="Transit RTT"
            sort={sort}
            onSortChange={onSortChange}
          />
        </HeaderCell>
        <HeaderCell className="text-right" aria-sort={ariaSortFor(sort, "transit_loss_ratio")}>
          <SortableHeader
            col="transit_loss_ratio"
            label="Transit loss"
            sort={sort}
            onSortChange={onSortChange}
          />
        </HeaderCell>
        <HeaderCell className="text-right" aria-sort={ariaSortFor(sort, "improvement_ms")}>
          <SortableHeader
            col="improvement_ms"
            label="Δ ms"
            sort={sort}
            onSortChange={onSortChange}
          />
        </HeaderCell>
        <HeaderCell className="text-right">Δ %</HeaderCell>
        <HeaderCell aria-sort={ariaSortFor(sort, "qualifies")}>
          <SortableHeader
            col="qualifies"
            label="Qualifies"
            sort={sort}
            onSortChange={onSortChange}
          />
        </HeaderCell>
        <HeaderCell aria-label="MTR">MTR</HeaderCell>
      </div>
      <div
        ref={scrollRef}
        className="relative overflow-auto"
        style={{ maxHeight: SCROLL_MAX_HEIGHT }}
        data-testid="candidate-pair-scroll-container"
      >
        <div style={{ position: "relative", height: `${totalSize}px` }}>
          {virtualItems.map((virtualItem) => {
            const row = rows[virtualItem.index];
            if (!row) return null;
            const key = `${row.source_agent_id}::${row.destination_agent_id}::${virtualItem.index}`;
            return (
              <PairRow
                key={key}
                row={row}
                index={virtualItem.index}
                top={virtualItem.start}
                height={virtualItem.size}
                agentsById={agentsById}
                onOpenMtr={onOpenMtr}
              />
            );
          })}
        </div>
        {isFetchingNextPage ? (
          <div
            role="status"
            aria-live="polite"
            className="border-t px-3 py-2 text-xs text-muted-foreground"
          >
            Loading more…
          </div>
        ) : null}
      </div>
    </div>
  );
}

function ariaSortFor(
  sort: SortState<PairDetailSortCol>,
  col: PairDetailSortCol,
): "ascending" | "descending" | "none" {
  if (sort.col !== col) return "none";
  return sort.dir === "asc" ? "ascending" : "descending";
}

// ---------------------------------------------------------------------------
// Header cell
// ---------------------------------------------------------------------------

interface HeaderCellProps {
  children: React.ReactNode;
  className?: string;
  "aria-label"?: string;
  "aria-sort"?: "ascending" | "descending" | "none";
}

function HeaderCell(props: HeaderCellProps) {
  const { children, className, ...rest } = props;
  return (
    // biome-ignore lint/a11y/useSemanticElements: CSS grid row needs div children; see role="table" rationale.
    // biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role.
    <div role="columnheader" className={cn("px-2", className)} {...rest}>
      {children}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Row
// ---------------------------------------------------------------------------

interface PairRowProps {
  row: EvaluationPairDetail;
  index: number;
  top: number;
  height: number;
  agentsById: Map<string, AgentSummary>;
  onOpenMtr: (measurementId: number, label: string) => void;
}

function PairRow({ row, index, top, height, agentsById, onOpenMtr }: PairRowProps) {
  const sourceAgent = agentsById.get(row.source_agent_id);
  const destAgent = agentsById.get(row.destination_agent_id);
  const sourceLabel = agentLabel(sourceAgent, row.source_agent_id);
  const destLabel = agentLabel(destAgent, row.destination_agent_id);
  const sourceIp = sourceAgent?.ip;
  const destIp = destAgent?.ip;
  const deltaPct = formatDeltaPercent(row.improvement_ms, row.direct_rtt_ms);

  return (
    // biome-ignore lint/a11y/useSemanticElements: virtualized row uses CSS grid; see role="table" rationale.
    // biome-ignore lint/a11y/useFocusableInteractive: role="row" is a grouping role, not interactive.
    <div
      role="row"
      data-index={index}
      data-testid={`candidate-pair-row-${index}`}
      className="absolute top-0 left-0 grid w-full items-center border-b px-3 py-2 text-sm hover:bg-muted/40"
      style={{
        transform: `translateY(${top}px)`,
        height: `${height}px`,
        gridTemplateColumns: GRID_TEMPLATE,
      }}
    >
      <Cell>
        <div className="flex flex-col">
          <span className="truncate font-medium" title={sourceLabel}>
            {sourceLabel}
          </span>
          {sourceIp ? (
            <span className="truncate text-[10px] text-muted-foreground">
              <IpHostname ip={sourceIp} />
            </span>
          ) : null}
        </div>
      </Cell>
      <Cell>
        <div className="flex flex-col">
          <span className="truncate font-medium" title={destLabel}>
            {destLabel}
          </span>
          {destIp ? (
            <span className="truncate text-[10px] text-muted-foreground">
              <IpHostname ip={destIp} />
            </span>
          ) : null}
        </div>
      </Cell>
      <Cell className="text-right tabular-nums">
        <div className="flex flex-col items-end">
          <span>{formatMs(row.direct_rtt_ms)}</span>
          <span className="text-[10px] text-muted-foreground">
            ±{formatMs(row.direct_stddev_ms)}
          </span>
        </div>
      </Cell>
      <Cell className="text-right tabular-nums text-xs text-muted-foreground">
        {formatLossRatio(row.direct_loss_ratio)}
      </Cell>
      <Cell className="text-right tabular-nums">
        <div className="flex flex-col items-end">
          <span>{formatMs(row.transit_rtt_ms)}</span>
          <span className="text-[10px] text-muted-foreground">
            ±{formatMs(row.transit_stddev_ms)}
          </span>
        </div>
      </Cell>
      <Cell className="text-right tabular-nums text-xs text-muted-foreground">
        {formatLossRatio(row.transit_loss_ratio)}
      </Cell>
      <Cell className={cn("text-right tabular-nums", improvementClass(row.improvement_ms))}>
        {formatImprovement(row.improvement_ms)}
      </Cell>
      <Cell
        className={cn("text-right tabular-nums", improvementClass(row.improvement_ms))}
        data-testid={`candidate-pair-row-${index}-delta-pct`}
      >
        {deltaPct}
      </Cell>
      <Cell>
        <div className="flex flex-wrap items-center gap-1">
          {row.qualifies ? (
            <Badge
              variant="secondary"
              className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
            >
              qualifies
            </Badge>
          ) : (
            <Badge variant="outline">below gate</Badge>
          )}
          {row.winning_x_position === 1 ? (
            <Badge
              variant="outline"
              className="font-mono text-[10px]"
              data-testid={`winning-x-position-${index}`}
              aria-label="X is first hop"
            >
              X first (A → X → Y → B)
            </Badge>
          ) : row.winning_x_position === 2 ? (
            <Badge
              variant="outline"
              className="font-mono text-[10px]"
              data-testid={`winning-x-position-${index}`}
              aria-label="X is second hop"
            >
              X second (A → Y → X → B)
            </Badge>
          ) : null}
        </div>
      </Cell>
      <Cell>
        <div className="flex items-center gap-1">
          <MtrIconButton
            measurementId={row.mtr_measurement_id_ax}
            label={`MTR ${sourceLabel} → ${row.destination_ip}`}
            arrow="A→X"
            onOpen={onOpenMtr}
          />
          <MtrIconButton
            measurementId={row.mtr_measurement_id_xb}
            label={`MTR ${row.destination_ip} → ${destLabel}`}
            arrow="X→B"
            onOpen={onOpenMtr}
          />
        </div>
      </Cell>
    </div>
  );
}

function Cell({
  children,
  className,
  ...rest
}: { children: React.ReactNode; className?: string } & React.HTMLAttributes<HTMLDivElement>) {
  return (
    // biome-ignore lint/a11y/useSemanticElements: CSS grid cells must be divs; see role="table" rationale.
    <div
      role="cell"
      className={cn("overflow-hidden px-2", className)}
      style={{ minWidth: 0 }}
      {...rest}
    >
      {children}
    </div>
  );
}

interface MtrIconButtonProps {
  measurementId: number | null | undefined;
  label: string;
  arrow: string;
  onOpen: (measurementId: number, label: string) => void;
}

function MtrIconButton({ measurementId, label, arrow, onOpen }: MtrIconButtonProps) {
  if (measurementId === null || measurementId === undefined) {
    return (
      <Button
        type="button"
        size="sm"
        variant="ghost"
        className="h-6 px-2 text-[10px]"
        disabled
        aria-label={`${label} (unavailable)`}
        title={`${label} (unavailable)`}
      >
        {arrow}
      </Button>
    );
  }
  return (
    <Button
      type="button"
      size="sm"
      variant="ghost"
      className="h-6 px-2 text-[10px]"
      onClick={() => onOpen(measurementId, label)}
      aria-label={label}
      title={label}
    >
      {arrow}
    </Button>
  );
}
