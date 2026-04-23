import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { formatDistanceToNowStrict } from "date-fns";
import { useMemo, useState } from "react";
import { usePathOverview } from "@/api/hooks/path-overview";
import type { components } from "@/api/schema.gen";
import { AgentCard } from "@/components/AgentCard";
import { GrafanaPanel } from "@/components/GrafanaPanel";
import { HopDetailCard } from "@/components/HopDetailCard";
import { ProtocolToggle } from "@/components/ProtocolToggle";
import { RouteHistoryTable } from "@/components/RouteHistoryTable";
import { RouteTable } from "@/components/RouteTable";
import { RouteTopology } from "@/components/RouteTopology";
import { Sparkline } from "@/components/Sparkline";
import { TimeRangePicker } from "@/components/TimeRangePicker";
import { Skeleton } from "@/components/ui/skeleton";
import { MESHMON_PATH_DASHBOARD, PANEL_LOSS, PANEL_RTT, PANEL_STDDEV } from "@/lib/grafana-panels";
import { grafanaTimes, type TimeRangeKey } from "@/lib/time-range";

type HopJson = components["schemas"]["HopJson"];
type Protocol = "icmp" | "udp" | "tcp";

// Anything older than this marks the snapshot as stale — matches the threshold
// the overview API uses to compute `stale` for its own badge, kept here so the
// banner renders without waiting for a second round-trip.
const STALE_SNAPSHOT_MS = 30 * 60 * 1000;

/**
 * Convert the route-search params bag into the `{ range, from, to }` shape
 * consumed by grafanaTimes and the path-overview hook. Narrows `protocol`.
 */
interface PathDetailSearch {
  range: TimeRangeKey;
  from?: string;
  to?: string;
  protocol?: Protocol;
}

export default function PathDetail() {
  // `strict: false` makes these hooks pull from the closest match in the tree
  // instead of being tied to a specific route id. The app router mounts this
  // page under the `auth-guard` id, but component tests wire it directly
  // under a root route — so the runtime id differs. We re-assert the shape
  // with `PathDetailSearch` below; Zod has already validated the search at
  // the router boundary in production.
  const params = useParams({ strict: false }) as { source: string; target: string };
  const { source, target } = params;
  const search = useSearch({ strict: false }) as PathDetailSearch;
  const { range, from, to, protocol } = search;
  const navigate = useNavigate();

  const overview = usePathOverview({
    source,
    target,
    range,
    customFrom: from,
    customTo: to,
    protocol,
  });
  const [selectedHop, setSelectedHop] = useState<HopJson | null>(null);

  // Override-unaware auto-pick for the `(auto)` badge. The server's
  // `primary_protocol` honours the `?protocol=` override, so asking the
  // server would make the badge disappear whenever the user picks manually.
  // Mirror the server rule (`icmp > udp > tcp` over `latest_by_protocol`)
  // locally. Returns `undefined` when nothing has data so the badge doesn't
  // anchor to a protocol with no snapshots in window.
  const autoProtocol = useMemo<Protocol | undefined>(() => {
    const latest = overview.data?.latest_by_protocol;
    if (!latest) return undefined;
    for (const p of ["icmp", "udp", "tcp"] as const) {
      if (latest[p]) return p;
    }
    return undefined;
  }, [overview.data]);

  // `isPending && !data` is the genuine "never resolved yet" state in TanStack
  // Query v5 — when `placeholderData: keepPreviousData` is set, subsequent
  // refetches flip `isFetching` but keep `data` defined, so we must not gate
  // on `isLoading` / `isPending` alone or every protocol/range toggle would
  // blank the page.
  if (overview.isPending && !overview.data) {
    return <Skeleton className="h-64 w-full" data-testid="path-detail-skeleton" />;
  }
  // A transient error during a background refetch must not replace the page
  // either — only surface the error view when we have nothing to show.
  if (overview.isError && !overview.data) {
    return (
      <p role="alert" className="p-6 text-sm text-destructive">
        Failed to load path overview
      </p>
    );
  }
  if (!overview.data) {
    return <Skeleton className="h-64 w-full" data-testid="path-detail-skeleton" />;
  }

  const data = overview.data;
  // `primary_protocol` is null when the pair has no snapshots in the window.
  // Don't fabricate ICMP — that would mislead the "Primary protocol: …" line
  // and inject `protocol=icmp` into Grafana vars and report links for data
  // that doesn't exist. Render an empty state instead; the toggle and range
  // picker stay so users can adjust without leaving the page.
  const effectiveProtocol = (data.primary_protocol ?? null) as Protocol | null;
  const latest = effectiveProtocol ? data.latest_by_protocol[effectiveProtocol] : null;
  const stale = latest ? Date.now() - Date.parse(latest.observed_at) > STALE_SNAPSHOT_MS : false;
  const gt = grafanaTimes(
    range,
    from && to ? { from: new Date(from), to: new Date(to) } : undefined,
  );
  const vars: Record<string, string> = effectiveProtocol
    ? { source, target, protocol: effectiveProtocol }
    : { source, target };

  return (
    <div className="p-6 flex flex-col gap-6">
      <header className="grid gap-3 md:grid-cols-2">
        <AgentCard agent={data.source} compact />
        <AgentCard agent={data.target} compact />
      </header>

      <section className="flex flex-wrap items-center justify-between gap-3">
        <div className="text-sm text-muted-foreground flex flex-wrap items-center gap-3">
          <span>
            Primary protocol:{" "}
            <span className="uppercase font-semibold">{effectiveProtocol ?? "—"}</span>
          </span>
          {latest && (
            <span title={latest.observed_at}>
              last observed{" "}
              {formatDistanceToNowStrict(new Date(latest.observed_at), { addSuffix: true })}
            </span>
          )}
          <span className="inline-flex items-center gap-2">
            <span>RTT</span>
            <Sparkline
              samples={(data.metrics?.rtt_series ?? []) as Array<[number, number]>}
              ariaLabel="RTT trend"
            />
            <span className="font-semibold">
              {data.metrics?.rtt_current != null
                ? `${data.metrics.rtt_current.toFixed(1)} ms`
                : "n/a"}
            </span>
          </span>
          <span className="inline-flex items-center gap-2">
            <span>Loss</span>
            <Sparkline
              samples={(data.metrics?.loss_series ?? []) as Array<[number, number]>}
              ariaLabel="Loss trend"
            />
            <span className="font-semibold">
              {data.metrics?.loss_current != null
                ? `${(data.metrics.loss_current * 100).toFixed(2)}%`
                : "n/a"}
            </span>
          </span>
          {overview.isFetching && (
            <span className="text-xs text-muted-foreground">refreshing…</span>
          )}
        </div>
        <div className="flex items-center gap-3">
          <ProtocolToggle
            value={effectiveProtocol}
            autoValue={autoProtocol}
            onChange={(p) =>
              navigate({
                to: "/paths/$source/$target",
                params: { source, target },
                search: { range, from, to, protocol: p },
              })
            }
          />
          <TimeRangePicker
            value={range}
            from={from}
            to={to}
            onChange={(next) => {
              // Custom mode requires both bounds — the router schema
              // rejects empty strings, so an intermediate edit that
              // clears a datetime-local field would throw inside
              // validateSearch and silently lose the edit. Drop the
              // transient invalid state; the next complete edit takes
              // effect normally.
              if (next.range === "custom" && (!next.from || !next.to)) return;
              navigate({
                to: "/paths/$source/$target",
                params: { source, target },
                search: { range: next.range, from: next.from, to: next.to, protocol },
              });
            }}
          />
        </div>
      </section>

      {stale && latest && (
        <p className="rounded border border-yellow-500/50 bg-yellow-500/10 p-2 text-sm">
          Latest snapshot is {formatDistanceToNowStrict(new Date(latest.observed_at))} old — data
          may be stale.
        </p>
      )}

      <section className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
        <GrafanaPanel
          dashboard={MESHMON_PATH_DASHBOARD}
          panelId={PANEL_RTT}
          vars={vars}
          from={gt.from}
          to={gt.to}
          title="RTT"
        />
        <GrafanaPanel
          dashboard={MESHMON_PATH_DASHBOARD}
          panelId={PANEL_LOSS}
          vars={vars}
          from={gt.from}
          to={gt.to}
          title="Loss"
        />
        <GrafanaPanel
          dashboard={MESHMON_PATH_DASHBOARD}
          panelId={PANEL_STDDEV}
          vars={vars}
          from={gt.from}
          to={gt.to}
          title="Stddev"
        />
      </section>

      {/*
        Grid must use `minmax(0,1fr)` for the graph column, not plain `1fr`.
        Cytoscape's container reports a min-content width that defeats `1fr`'s
        default `minmax(auto, 1fr)` and makes the whole section overflow the
        viewport when the hop card appears. `minmax(0,1fr)` lets the column
        shrink freely; the card keeps its natural width via `w-80 shrink-0`.
      */}
      <section className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto] items-start">
        <div className="min-w-0">
          <h2 className="mb-2 text-lg font-semibold">Current route</h2>
          <RouteTopology
            hops={latest?.hops ?? []}
            onNodeClick={setSelectedHop}
            ariaLabel="Current route topology"
          />
        </div>
        {selectedHop && (
          <HopDetailCard
            hop={selectedHop}
            onClose={() => setSelectedHop(null)}
            className="w-80 shrink-0 self-start"
          />
        )}
      </section>

      {latest && latest.hops.length > 0 && (
        <section>
          <h2 className="mb-2 text-lg font-semibold">Current route hops</h2>
          <RouteTable hops={latest.hops} />
        </section>
      )}

      <section>
        <h2 className="mb-2 text-lg font-semibold">Route change history</h2>
        <RouteHistoryTable
          snapshots={data.recent_snapshots}
          truncated={data.recent_snapshots_truncated}
          onCompare={({ a, b }) =>
            navigate({
              to: "/paths/$source/$target/routes/compare",
              params: { source, target },
              search: { a, b },
            })
          }
        />
      </section>

      {effectiveProtocol && (
        <div>
          <Link
            to="/reports/path"
            search={{
              source_id: source,
              target_id: target,
              from: data.window.from,
              to: data.window.to,
              protocol: effectiveProtocol,
            }}
            className="inline-block text-sm underline underline-offset-2"
          >
            Generate report
          </Link>
        </div>
      )}
    </div>
  );
}
