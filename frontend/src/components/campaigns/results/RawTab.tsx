/**
 * Raw tab — virtualized feed over `GET /api/campaigns/:id/measurements`.
 *
 * Columns: source (agent display name), destination (IP), protocol, kind,
 * measured_at (relative + absolute via `title`), loss_ratio
 * (color-chipped), MTR link. Pending / dispatched rows land here alongside
 * settled measurements so the operator can watch in-flight detail work.
 *
 * Filter state lives on the URL via `campaignDetailSearchSchema.raw_*` so a
 * shared link reproduces the view. Other search params (sibling tabs'
 * sort/filter, `tab` itself) survive filter edits — the `...search` merge
 * pattern is load-bearing here. Virtualization mirrors `CatalogueTable`'s
 * scroll-element + overscan setup; `fetchNextPage` fires when the
 * virtualizer's last rendered row crosses the tail of the currently-loaded
 * page set.
 */

import { useNavigate, useSearch } from "@tanstack/react-router";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { type AgentSummary, useAgents } from "@/api/hooks/agents";
import type {
  Campaign,
  CampaignMeasurement,
  MeasurementKind,
  PairResolutionState,
  ProbeProtocol,
} from "@/api/hooks/campaigns";
import { useCampaignMeasurements, useForcePair } from "@/api/hooks/campaigns";
import { RawFilterBar, type RawFilterSelection } from "@/components/campaigns/results/RawFilterBar";
import { IpHostname } from "@/components/ip-hostname";
import { RouteTopology } from "@/components/RouteTopology";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { extractCampaignErrorCode, isIllegalStateTransition, isMissingPair } from "@/lib/campaign";
import { formatLossRatio, LOSS_RATIO_THRESHOLDS } from "@/lib/format-loss";
import {
  type MeasurementSource,
  measurementSourceBadgeClass,
  measurementSourceLabel,
  normaliseSource,
} from "@/lib/measurement-source";
import { formatRelativeAgo } from "@/lib/time-format";
import { cn } from "@/lib/utils";
import type { CampaignDetailSearch } from "@/router/index";
import { useToastStore } from "@/stores/toast";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/** Row height estimate in px — keep in sync with the grid row class. The
 * Kind cell stacks three lines (kind, state, source badge); the virtualizer
 * re-measures every row after layout, so the estimate just needs to be close
 * enough that the first render's overscan stays correct. */
const ROW_HEIGHT_ESTIMATE = 56;

/** Max height of the virtualized scroll container. */
const SCROLL_MAX_HEIGHT = "60vh";

/**
 * Initial viewport rect fed to the virtualizer before the ResizeObserver
 * fires. jsdom never measures real layout; a generous default keeps the
 * test renderer populated while the real observer supersedes it within a
 * frame in production.
 */
const INITIAL_SCROLL_RECT = { width: 1024, height: 600 };

/** Grid track template — fixed widths for dense columns, `fr` for wide text. */
const GRID_TEMPLATE =
  "minmax(180px, 1.2fr) minmax(140px, 1fr) 80px 110px minmax(140px, 1fr) 100px 90px 90px 80px";

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface RawTabProps {
  campaign: Campaign;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function RawTab({ campaign }: RawTabProps) {
  const navigate = useNavigate();
  // `strict: false` keeps the hook usable under the component-test router
  // tree. The registered route's `validateSearch` already normalised the
  // shape so the cast is safe.
  const search = useSearch({ strict: false }) as CampaignDetailSearch;

  const selection: RawFilterSelection = useMemo(
    () => ({
      resolution_state: search.raw_state,
      protocol: search.raw_protocol,
      kind: search.raw_kind,
    }),
    [search.raw_state, search.raw_protocol, search.raw_kind],
  );

  const onSelectionChange = useCallback(
    (delta: Partial<RawFilterSelection>): void => {
      const navigateSearch = navigate as unknown as (opts: {
        search: CampaignDetailSearch;
        replace: boolean;
      }) => void;
      navigateSearch({
        search: {
          ...search,
          raw_state:
            "resolution_state" in delta ? (delta.resolution_state ?? undefined) : search.raw_state,
          raw_protocol: "protocol" in delta ? (delta.protocol ?? undefined) : search.raw_protocol,
          raw_kind: "kind" in delta ? (delta.kind ?? undefined) : search.raw_kind,
        },
        replace: true,
      });
    },
    [navigate, search],
  );

  const measurementsQuery = useCampaignMeasurements(campaign.id, {
    resolution_state: selection.resolution_state,
    protocol: selection.protocol,
    kind: selection.kind,
  });

  const agentsQuery = useAgents();
  const agentsById = useMemo<Map<string, AgentSummary>>(() => {
    const map = new Map<string, AgentSummary>();
    for (const agent of agentsQuery.data ?? []) {
      map.set(agent.id, agent);
    }
    return map;
  }, [agentsQuery.data]);

  const rows = useMemo<CampaignMeasurement[]>(
    () => measurementsQuery.data?.pages.flatMap((p) => p.entries) ?? [],
    [measurementsQuery.data],
  );

  // The backend's keyset cursor walks settled rows (`measured_at IS NOT NULL`)
  // and emits `next_cursor` only when the last row on a page has
  // `measured_at`. If a page saturates inside the pending-row tail, the
  // remainder of the pending tail is unreachable — the footer must not
  // claim "End of feed" without pointing the operator at the state filter
  // that actually enumerates in-flight work.
  const hasPendingTail = useMemo(
    () => rows.some((r) => r.resolution_state === "pending" || r.resolution_state === "dispatched"),
    [rows],
  );

  const [activeMtrRow, setActiveMtrRow] = useState<CampaignMeasurement | null>(null);

  // -------------------------------------------------------------------------
  // Row actions
  // -------------------------------------------------------------------------

  const handleViewHistory = useCallback(
    (row: CampaignMeasurement): void => {
      const navigateHistory = navigate as unknown as (opts: {
        to: string;
        search: { source: string; destination: string };
      }) => void;
      navigateHistory({
        to: "/history/pair",
        search: { source: row.source_agent_id, destination: row.destination_ip },
      });
    },
    [navigate],
  );

  const forcePairMutation = useForcePair();
  const handleForcePair = useCallback(
    (row: CampaignMeasurement): void => {
      const { pushToast } = useToastStore.getState();
      forcePairMutation.mutate(
        {
          id: campaign.id,
          body: { source_agent_id: row.source_agent_id, destination_ip: row.destination_ip },
        },
        {
          onSuccess: () => {
            pushToast({
              kind: "success",
              message: `Queued force re-measure for ${row.destination_ip}.`,
            });
          },
          onError: (err) => {
            if (isIllegalStateTransition(err)) {
              pushToast({
                kind: "error",
                message: "Can't force pair — campaign advanced before the request landed.",
              });
              return;
            }
            if (isMissingPair(err)) {
              pushToast({
                kind: "error",
                message: `Pair ${row.destination_ip} no longer exists on this campaign.`,
              });
              return;
            }
            const code = extractCampaignErrorCode(err);
            pushToast({
              kind: "error",
              message: code ? `Force pair failed: ${code}` : `Force pair failed: ${err.message}`,
            });
          },
        },
      );
    },
    [forcePairMutation, campaign.id],
  );

  // -------------------------------------------------------------------------
  // Virtualizer
  // -------------------------------------------------------------------------

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

  // Auto-fetch: when the last rendered virtual row approaches the tail of
  // the currently-loaded page set, kick the next page. The callback is a
  // cheap no-op if `hasNextPage` is false, so it's safe to over-fire.
  // Destructure the infinite-query fields the effect reads — react-query does
  // not guarantee reference-stable result objects across unrelated flag
  // changes, so depending on `measurementsQuery` as a whole would re-run the
  // effect on every refetch tick; listing the three fields keeps the
  // dependency surface tight.
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = measurementsQuery;
  useEffect(() => {
    if (!hasNextPage || isFetchingNextPage) return;
    const lastItem = virtualItems[virtualItems.length - 1];
    if (!lastItem) return;
    if (lastItem.index >= rows.length - 5) {
      void fetchNextPage();
    }
  }, [virtualItems, rows.length, hasNextPage, isFetchingNextPage, fetchNextPage]);

  // Clock ticks "just now → 45s → …" — driver lives here so every row
  // re-renders in lockstep without individual `useInterval` hooks.
  const [nowMs, setNowMs] = useState<number>(() => Date.now());
  useEffect(() => {
    const handle = window.setInterval(() => setNowMs(Date.now()), 30_000);
    return () => window.clearInterval(handle);
  }, []);

  return (
    <section data-testid="raw-tab" className="flex flex-col gap-3">
      <RawFilterBar selection={selection} onChange={onSelectionChange} />

      {measurementsQuery.isError ? (
        <Card className="p-4 text-sm text-destructive" role="alert">
          Failed to load measurements: {measurementsQuery.error?.message ?? "unknown error"}
        </Card>
      ) : measurementsQuery.isLoading ? (
        <div role="status" aria-live="polite" className="flex flex-col gap-2">
          <span className="sr-only">Loading measurements…</span>
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-full" />
        </div>
      ) : rows.length === 0 ? (
        <Card className="p-6 text-sm text-muted-foreground" role="status">
          No measurements match the current filters.
        </Card>
      ) : (
        // biome-ignore lint/a11y/useSemanticElements: virtualized row/grid rendering requires CSS grid on every row; switching to <table> forces `table-layout: fixed` which breaks the `fr` tracks — same rationale as CatalogueTable.
        <div className="rounded-md border" role="table" aria-label="Raw measurements">
          {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale above. */}
          {/* biome-ignore lint/a11y/useFocusableInteractive: role="row" is a grouping role in the ARIA table pattern — not an interactive control. */}
          <div
            role="row"
            className="grid w-full border-b bg-muted/30 px-3 py-2 text-xs font-medium uppercase tracking-wide text-muted-foreground"
            style={{ gridTemplateColumns: GRID_TEMPLATE }}
          >
            <HeaderCell>Source</HeaderCell>
            <HeaderCell>Destination</HeaderCell>
            <HeaderCell>Protocol</HeaderCell>
            <HeaderCell>Kind</HeaderCell>
            <HeaderCell>Measured</HeaderCell>
            <HeaderCell className="text-right">Latency avg</HeaderCell>
            <HeaderCell className="text-right">Loss</HeaderCell>
            <HeaderCell>MTR</HeaderCell>
            <HeaderCell aria-label="Actions">&nbsp;</HeaderCell>
          </div>
          <div
            ref={scrollRef}
            className="relative overflow-auto"
            style={{ maxHeight: SCROLL_MAX_HEIGHT }}
            data-testid="raw-scroll-container"
          >
            <div style={{ position: "relative", height: `${totalSize}px` }}>
              {virtualItems.map((virtualItem) => {
                const row = rows[virtualItem.index];
                if (!row) return null;
                const key = `${row.pair_id}:${row.measurement_id ?? "pending"}`;
                return (
                  <MeasurementRow
                    key={key}
                    row={row}
                    index={virtualItem.index}
                    top={virtualItem.start}
                    height={virtualItem.size}
                    agentsById={agentsById}
                    nowMs={nowMs}
                    onOpenMtr={(r) => setActiveMtrRow(r)}
                    onForcePair={handleForcePair}
                    onViewHistory={handleViewHistory}
                  />
                );
              })}
            </div>
            {measurementsQuery.isFetchingNextPage ? (
              <div
                role="status"
                aria-live="polite"
                className="border-t px-3 py-2 text-xs text-muted-foreground"
              >
                Loading more…
              </div>
            ) : null}
          </div>
          {!measurementsQuery.hasNextPage && rows.length > 0 ? (
            <footer className="border-t px-3 py-2 text-xs text-muted-foreground">
              {hasPendingTail && selection.resolution_state === undefined ? (
                <>
                  End of settled measurements — {rows.length.toLocaleString()} rows shown. Narrow by{" "}
                  <strong>Resolution state: pending</strong> or <strong>dispatched</strong> to
                  enumerate in-flight work.
                </>
              ) : (
                <>End of feed — {rows.length.toLocaleString()} rows.</>
              )}
            </footer>
          ) : null}
        </div>
      )}

      {activeMtrRow ? (
        <MtrPreview
          campaign={campaign}
          row={activeMtrRow}
          agentsById={agentsById}
          onClose={() => setActiveMtrRow(null)}
        />
      ) : null}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Header cell
// ---------------------------------------------------------------------------

interface HeaderCellProps {
  children: React.ReactNode;
  className?: string;
  "aria-label"?: string;
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

interface MeasurementRowProps {
  row: CampaignMeasurement;
  index: number;
  top: number;
  height: number;
  agentsById: Map<string, AgentSummary>;
  nowMs: number;
  onOpenMtr: (row: CampaignMeasurement) => void;
  onForcePair: (row: CampaignMeasurement) => void;
  onViewHistory: (row: CampaignMeasurement) => void;
}

function MeasurementRow({
  row,
  index,
  top,
  height,
  agentsById,
  nowMs,
  onOpenMtr,
  onForcePair,
  onViewHistory,
}: MeasurementRowProps) {
  const sourceAgent = agentsById.get(row.source_agent_id);
  const sourceLabel = sourceAgent?.display_name || row.source_agent_id;
  const measuredLabel = row.measured_at
    ? formatRelativeAgo(new Date(row.measured_at).getTime(), nowMs)
    : "—";

  return (
    // biome-ignore lint/a11y/useSemanticElements: virtualized row uses CSS grid; see role="table" rationale.
    // biome-ignore lint/a11y/useFocusableInteractive: role="row" is a grouping role, not interactive.
    <div
      role="row"
      data-index={index}
      data-testid={`raw-row-${index}`}
      className="absolute top-0 left-0 grid w-full items-center border-b px-3 py-2 text-sm hover:bg-muted/40"
      style={{
        transform: `translateY(${top}px)`,
        height: `${height}px`,
        gridTemplateColumns: GRID_TEMPLATE,
      }}
    >
      <Cell>
        <div className="flex flex-col">
          <span className="truncate font-medium">{sourceLabel}</span>
          <span className="truncate font-mono text-[10px] text-muted-foreground">
            {row.source_agent_id}
          </span>
        </div>
      </Cell>
      <Cell>
        <span className="truncate text-xs">
          <IpHostname ip={row.destination_ip} />
        </span>
      </Cell>
      <Cell>{row.protocol ? <ProtocolBadge protocol={row.protocol} /> : "—"}</Cell>
      <Cell>
        <KindBadge kind={row.pair_kind} state={row.resolution_state} source={row.source} />
      </Cell>
      <Cell>
        <span
          className="truncate text-xs text-muted-foreground"
          title={row.measured_at ?? undefined}
        >
          {measuredLabel}
        </span>
      </Cell>
      <Cell className="text-right tabular-nums">{formatLatency(row.latency_avg_ms)}</Cell>
      <Cell className="text-right">
        <LossChip value={row.loss_ratio} />
      </Cell>
      <Cell>
        {row.mtr_id ? (
          <Button
            type="button"
            size="sm"
            variant="outline"
            className="h-6 px-2 text-xs"
            onClick={() => onOpenMtr(row)}
            aria-label={`Open MTR for ${row.destination_ip}`}
          >
            MTR
          </Button>
        ) : (
          <span className="text-xs text-muted-foreground">—</span>
        )}
      </Cell>
      <Cell>
        <div className="flex items-center gap-1">
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="h-6 w-6 p-0 text-xs"
            onClick={() => onForcePair(row)}
            aria-label={`Force re-measure ${row.source_agent_id} → ${row.destination_ip}`}
            title="Force re-measure"
          >
            ↻
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="h-6 w-6 p-0 text-xs"
            onClick={() => onViewHistory(row)}
            aria-label={`View history for ${row.source_agent_id} → ${row.destination_ip}`}
            title="View history"
            data-testid={`raw-row-${index}-history`}
          >
            ⧉
          </Button>
        </div>
      </Cell>
    </div>
  );
}

function Cell({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    // biome-ignore lint/a11y/useSemanticElements: CSS grid cells must be divs; see role="table" rationale.
    <div role="cell" className={cn("overflow-hidden px-2", className)} style={{ minWidth: 0 }}>
      {children}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Formatters + small badges
// ---------------------------------------------------------------------------

function formatLatency(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  return `${value.toFixed(1)} ms`;
}

function ProtocolBadge({ protocol }: { protocol: ProbeProtocol }) {
  return (
    <Badge variant="outline" className="font-mono text-[10px] uppercase">
      {protocol}
    </Badge>
  );
}

function KindBadge({
  kind,
  state,
  source,
}: {
  kind: MeasurementKind;
  state: PairResolutionState;
  source: MeasurementSource | null | undefined;
}) {
  const resolvedSource = normaliseSource(source);
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-xs">{kind}</span>
      <span className="text-[10px] text-muted-foreground">{state}</span>
      <Badge
        variant="outline"
        className={cn(
          "w-fit px-1 py-0 font-mono text-[9px] uppercase tracking-wide",
          measurementSourceBadgeClass(resolvedSource),
        )}
        data-testid={`raw-source-badge-${resolvedSource}`}
        aria-label={`Source: ${measurementSourceLabel(resolvedSource)}`}
      >
        {measurementSourceLabel(resolvedSource)}
      </Badge>
    </div>
  );
}

function LossChip({ value }: { value: number | null | undefined }) {
  if (value === null || value === undefined) {
    return <span className="text-xs text-muted-foreground">—</span>;
  }
  // `value` is wire-format ratio (0.0–1.0); thresholds are expressed in the
  // same units (0.5% → 0.005, 2% → 0.02). `formatLossRatio` multiplies by
  // 100 at display.
  const klass =
    value < LOSS_RATIO_THRESHOLDS.healthy
      ? "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
      : value < LOSS_RATIO_THRESHOLDS.degraded
        ? "bg-amber-500/15 text-amber-700 dark:text-amber-300"
        : "bg-destructive/15 text-destructive";
  return (
    <Badge variant="outline" className={cn("font-mono text-[10px]", klass)}>
      {formatLossRatio(value)}
    </Badge>
  );
}

// ---------------------------------------------------------------------------
// MTR preview panel — lazy single-row fetch via `measurement_id`
// ---------------------------------------------------------------------------

interface MtrPreviewProps {
  campaign: Campaign;
  row: CampaignMeasurement;
  agentsById: Map<string, AgentSummary>;
  onClose: () => void;
}

function MtrPreview({ campaign, row, agentsById, onClose }: MtrPreviewProps) {
  // `measurement_id` is guaranteed non-null at call time (gated by the row's
  // `mtr_id` check), but the schema types it nullable — narrow defensively.
  const measurementId = row.measurement_id ?? 0;
  const query = useCampaignMeasurements(campaign.id, { measurement_id: measurementId, limit: 1 });
  const target = query.data?.pages[0]?.entries[0];
  const hops = target?.mtr_hops ?? null;
  const sourceLabel = agentsById.get(row.source_agent_id)?.display_name || row.source_agent_id;
  const label = `${sourceLabel} → ${row.destination_ip}`;

  return (
    <Card className="flex flex-col gap-3 p-3" aria-label="MTR preview">
      <header className="flex items-center justify-between">
        <h3 className="text-sm font-semibold">MTR · {label}</h3>
        <Button type="button" size="sm" variant="ghost" onClick={onClose}>
          Close
        </Button>
      </header>
      {query.isLoading ? (
        <div className="text-xs text-muted-foreground" role="status">
          Loading MTR hops…
        </div>
      ) : query.isError ? (
        <div className="text-xs text-destructive" role="alert">
          Failed to load hops: {query.error?.message ?? "unknown error"}
        </div>
      ) : !target ? (
        <div className="text-xs text-muted-foreground" role="status">
          The MTR measurement has not settled yet.
        </div>
      ) : !hops || hops.length === 0 ? (
        <div className="text-xs text-muted-foreground" role="status">
          No hop data captured for this measurement.
        </div>
      ) : (
        <div className="h-[280px]">
          <RouteTopology hops={hops} ariaLabel={`${label} hops`} className="h-full" />
        </div>
      )}
    </Card>
  );
}
