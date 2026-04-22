// TODO(T47-followup): city/country/ASN columns are blank until AgentSummary
// carries catalogue-joined fields. See crates/service/src/http/agents.rs +
// docs/superpowers/plans/meshmon/detail-plans/T47-plan.md F.2 line 503.
import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentSummary } from "@/api/hooks/agents";
import { useAgents } from "@/api/hooks/agents";
import { useAgentLivenessThresholds } from "@/api/hooks/liveness";
import type { components } from "@/api/schema.gen";
import type { FilterValue } from "@/components/filter/FilterRail";
import { FilterRail } from "@/components/filter/FilterRail";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { pointInShapes } from "@/lib/geo";
import { type AgentLivenessState, getAgentLiveness } from "@/lib/health";
import { cn } from "@/lib/utils";

type FacetsResponse = components["schemas"]["FacetsResponse"];

/** Virtualized row height — matches shadcn TableRow default. */
const ROW_HEIGHT = 44;

/** Max height of the virtualized scroll viewport. */
const SCROLL_MAX_HEIGHT = "60vh";

/**
 * Initial viewport rect fed to the virtualizer before its ResizeObserver
 * fires. jsdom never measures layout, so a zero rect would collapse the
 * virtualizer to nothing and leave tests with no rendered rows.
 */
const INITIAL_SCROLL_RECT = { width: 1024, height: 600 };

const OFFLINE_TOOLTIP =
  "This agent is currently offline — its pairs will be skipped after 3 attempts.";
const STALE_TOOLTIP =
  "Agent push is in-flight — the registry snapshot may be one refresh tick behind.";

/**
 * Shared CSS grid track expression for the header + virtualized body.
 * Columns: ID, Name, City, Country, ASN, Status, selected-check.
 */
const GRID_TEMPLATE = "100px minmax(160px, 1.5fr) 120px 110px 90px 110px 32px";

export interface SourcePanelProps {
  selected: Set<string>;
  onSelectedChange(next: Set<string>): void;
  filter: FilterValue;
  onFilterChange(next: FilterValue): void;
  facets: FacetsResponse | undefined;
  onOpenMap(): void;
  /**
   * When true, every mutation button (Add all / Add matching / Remove all /
   * Map view) is disabled and row-click no-ops. The filter rail stays live
   * so the operator can still inspect matches, but selection is frozen —
   * composer sets this after a draft campaign has been created so further
   * source edits don't silently diverge from the persisted draft.
   */
  disabled?: boolean;
}

/**
 * Does the agent satisfy every facet in `filter`?
 *
 * Shape filters are point-in-polygon against `catalogue_coordinates`;
 * agents without coordinates are excluded entirely (never zero-defaulted)
 * when any shape is active. ASN / network / country / city facets are
 * matched against catalogue-joined fields that the API may not populate
 * on agents today — they only restrict the set when the operator
 * explicitly picked a value.
 */
function matchesFilter(agent: AgentSummary, filter: FilterValue): boolean {
  // Shape filter: point-in-polygon. Agents without coordinates fall out.
  if (filter.shapes.length > 0) {
    if (!agent.catalogue_coordinates) return false;
    const { latitude, longitude } = agent.catalogue_coordinates;
    if (!pointInShapes(latitude, longitude, filter.shapes)) return false;
  }

  // Free-text name search matches either id or display_name.
  if (filter.nameSearch) {
    const needle = filter.nameSearch.toLowerCase();
    const hay = `${agent.id} ${agent.display_name}`.toLowerCase();
    if (!hay.includes(needle)) return false;
  }

  // IP-prefix match is a literal string prefix on the agent's ip. The
  // IP-prefix normalizer is catalogue-specific (CIDR expansion), so we
  // keep the agent path intentionally simple here — operators who want
  // IP filtering usually reach for it via the destination panel.
  if (filter.ipPrefix) {
    if (!agent.ip.startsWith(filter.ipPrefix)) return false;
  }

  // The remaining facets (country / asn / network / city) live on the
  // catalogue row, not on AgentSummary. Agents are joined to that row
  // by IP; the public AgentSummary type doesn't re-surface the fields
  // today, so we intentionally do not filter on them here. When those
  // fields are exposed on agents (T47 follow-ups) this function will
  // gain predicates for each.

  return true;
}

/**
 * Three-state liveness badge. Renders:
 * - `online` → muted "Online" badge
 * - `stale`  → soft warning badge (snapshot may be one refresh tick behind)
 * - `offline` → destructive badge with the offline tooltip
 *
 * Always emits an `aria-label` so screen-reader and the existing
 * `getByLabelText(/offline/i)` test selectors keep working.
 */
function LivenessBadge({ agentId, liveness }: { agentId: string; liveness: AgentLivenessState }) {
  if (liveness === "offline") {
    return (
      <Badge variant="destructive" aria-label={`Offline: ${agentId}`} title={OFFLINE_TOOLTIP}>
        Offline ⚠
      </Badge>
    );
  }
  if (liveness === "stale") {
    return (
      <Badge variant="outline" aria-label={`Stale: ${agentId}`} title={STALE_TOOLTIP}>
        Stale
      </Badge>
    );
  }
  return (
    <Badge variant="secondary" aria-label={`Online: ${agentId}`}>
      Online
    </Badge>
  );
}

interface AgentRow {
  agent: AgentSummary;
}

/**
 * Liveness re-render cadence. The `useAgents` query refetches every
 * 30 s; the default stale threshold is 20 s, so without an independent
 * tick a freshly-still agent could read as Online for up to 10 s past
 * the threshold. A 10 s `setInterval` bound to component lifetime
 * matches the same pattern used by `RawTab` for "just now → 45s → …"
 * cadence and forces re-rendering against a fresh `Date.now()`.
 */
const LIVENESS_TICK_MS = 10_000;

export function SourcePanel({
  selected,
  onSelectedChange,
  filter,
  onFilterChange,
  facets,
  onOpenMap,
  disabled = false,
}: SourcePanelProps) {
  const { data: agents } = useAgents();
  const livenessThresholds = useAgentLivenessThresholds();

  // Drive a re-render every `LIVENESS_TICK_MS` so badges transition
  // through the stale / offline thresholds even when no `useAgents`
  // refetch has landed. The state value is unused — only the setter
  // needs to be called to schedule a render. See the constant's doc
  // comment for the cadence rationale.
  const [, setTick] = useState(0);
  useEffect(() => {
    const handle = window.setInterval(() => setTick((n) => n + 1), LIVENESS_TICK_MS);
    return () => window.clearInterval(handle);
  }, []);

  // Liveness is computed inside the row render against `Date.now()` at
  // render time, NOT memoized over `agents`. Sampling `now` inside the
  // memo callback would freeze the badge to whichever snapshot
  // `useAgents` last returned — the registry snapshot itself can lag by
  // up to `refresh_interval_seconds` on the server, so a fresh push from
  // the agent might briefly look stale-or-older than the threshold even
  // though wall-clock time has actually moved on. Re-sampling per render
  // — combined with the `LIVENESS_TICK_MS` clock above — keeps the
  // badge in sync with the wall clock across both data refreshes and
  // quiet periods.
  const allRows: AgentRow[] = useMemo(() => (agents ?? []).map((agent) => ({ agent })), [agents]);

  const filteredRows: AgentRow[] = useMemo(
    () => allRows.filter((row) => matchesFilter(row.agent, filter)),
    [allRows, filter],
  );

  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: filteredRows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 8,
    initialRect: INITIAL_SCROLL_RECT,
  });

  const handleAddVisible = () => {
    if (disabled) return;
    const next = new Set(selected);
    for (const row of filteredRows) next.add(row.agent.id);
    onSelectedChange(next);
  };

  const handleRemoveAll = () => {
    if (disabled) return;
    if (selected.size === 0) return;
    onSelectedChange(new Set());
  };

  const handleToggleRow = (id: string) => {
    if (disabled) return;
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    onSelectedChange(next);
  };

  return (
    <section aria-label="Sources" className="flex h-full min-h-0 gap-3">
      <aside className="w-64 shrink-0 overflow-y-auto">
        <FilterRail
          value={filter}
          onChange={onFilterChange}
          facets={facets}
          onOpenMap={onOpenMap}
        />
      </aside>

      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <header className="flex flex-wrap items-center gap-2">
          <h2 className="text-base font-semibold">Sources</h2>
          <div className="ml-auto flex items-center gap-2">
            {/*
              Plan F.2 §506: "Add all" and "Add matching" share the same handler
              in F1 — both snapshot the currently filtered rows. Split if F2
              changes semantics.
            */}
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={handleAddVisible}
              disabled={disabled || filteredRows.length === 0}
            >
              Add all
            </Button>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={handleAddVisible}
              disabled={disabled || filteredRows.length === 0}
            >
              Add matching
            </Button>
            <Button
              type="button"
              size="sm"
              variant="ghost"
              onClick={handleRemoveAll}
              disabled={disabled || selected.size === 0}
            >
              Remove all
            </Button>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={onOpenMap}
              disabled={disabled}
            >
              Map view
            </Button>
          </div>
        </header>

        {/*
          Real `<table>` can't carry `fr` tracks (table-layout needs
          absolute widths) and would clip-past-right-edge on narrow
          viewports. We mirror CatalogueTable's div+grid pattern — ARIA
          roles preserve screen-reader semantics while CSS grid owns
          layout.
        */}
        {/* biome-ignore lint/a11y/useSemanticElements: see block comment above — a real <table> would force `table-layout: fixed` and break `fr` tracks. */}
        <div role="table" aria-label="Sources" className="flex flex-col rounded-md border">
          {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
          {/* biome-ignore lint/a11y/useFocusableInteractive: role="row" is a grouping role in the ARIA table pattern — not an interactive control. */}
          <div
            role="row"
            className="grid w-full border-b bg-muted/30 text-sm font-medium text-muted-foreground"
            style={{ gridTemplateColumns: GRID_TEMPLATE }}
          >
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              ID
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              Name
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              City
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              Country
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              ASN
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              Status
            </div>
            {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
            {/* biome-ignore lint/a11y/useFocusableInteractive: role="columnheader" is a non-interactive structural role. */}
            <div role="columnheader" className="px-3 py-2">
              &nbsp;
            </div>
          </div>

          <div
            ref={scrollRef}
            className="relative overflow-auto"
            style={{ maxHeight: SCROLL_MAX_HEIGHT }}
          >
            <div style={{ position: "relative", height: `${virtualizer.getTotalSize()}px` }}>
              {virtualizer.getVirtualItems().map((item) => {
                const row = filteredRows[item.index];
                if (!row) return null;
                const { agent } = row;
                // Liveness sampled here, NOT in the upstream memo, so a
                // late-arriving server snapshot doesn't pin a stale "now"
                // into the badge for the lifetime of that snapshot. See
                // the `allRows` comment for the full rationale.
                const liveness = getAgentLiveness(agent.last_seen_at, livenessThresholds);
                const isSelected = selected.has(agent.id);
                return (
                  /* biome-ignore lint/a11y/useSemanticElements: virtualized row is a CSS-grid parent; role="button" keeps the click+keyboard affordance. */
                  <div
                    key={agent.id}
                    role="button"
                    tabIndex={0}
                    aria-pressed={isSelected}
                    aria-label={`Toggle source ${agent.id}`}
                    onClick={() => handleToggleRow(agent.id)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        handleToggleRow(agent.id);
                      }
                    }}
                    className={cn(
                      "absolute top-0 left-0 grid w-full cursor-pointer items-center overflow-hidden border-b text-sm hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none",
                      isSelected && "bg-primary/10",
                    )}
                    style={{
                      transform: `translateY(${item.start}px)`,
                      height: `${item.size}px`,
                      gridTemplateColumns: GRID_TEMPLATE,
                    }}
                  >
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3 font-mono text-xs" title={agent.id}>
                      {agent.id.slice(0, 8)}
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3">
                      <span className="block truncate">{agent.display_name}</span>
                      <span className="block truncate font-mono text-[10px] text-muted-foreground">
                        {agent.ip}
                      </span>
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3 text-muted-foreground">
                      —
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3 text-muted-foreground">
                      —
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="truncate px-3 text-muted-foreground">
                      —
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="px-3">
                      <LivenessBadge agentId={agent.id} liveness={liveness} />
                    </div>
                    {/* biome-ignore lint/a11y/useSemanticElements: see role="table" rationale. */}
                    <div role="cell" className="px-3 text-muted-foreground">
                      {isSelected ? "✓" : ""}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>

          {filteredRows.length === 0 ? (
            <p className="px-4 py-6 text-center text-sm text-muted-foreground">
              No agents match the current filter.
            </p>
          ) : null}

          <footer className="flex items-center justify-between gap-3 border-t bg-muted/20 px-4 py-2 text-xs text-muted-foreground">
            <span>
              {filteredRows.length} of {allRows.length} agents match
            </span>
            <Badge variant="secondary" aria-label="Selected sources count">
              {selected.size} sources selected
            </Badge>
          </footer>
        </div>
      </div>
    </section>
  );
}
