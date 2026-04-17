import { useSearch } from "@tanstack/react-router";
import { useMemo } from "react";
import { usePathOverview } from "@/api/hooks/path-overview";
import { useRouteSnapshot } from "@/api/hooks/route-snapshot";
import { GrafanaPanel } from "@/components/GrafanaPanel";
import { RouteTable, type RouteTableDiff } from "@/components/RouteTable";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { MESHMON_PATH_DASHBOARD, PANEL_LOSS, PANEL_RTT } from "@/lib/grafana-panels";
import { buildReportSummary, type MetricsPoint } from "@/lib/report-summary";
import { computeRouteDiff } from "@/lib/route-diff";

interface ReportSearch {
  source_id: string;
  target_id: string;
  from: string;
  to: string;
  protocol?: "icmp" | "udp" | "tcp";
}

/**
 * Format a Date as `YYYY-MM-DD HH:mm UTC` with real UTC wall-clock time.
 * date-fns' `format` honours the browser's local timezone, which silently
 * mislabels displayed timestamps when the label says 'UTC'. ISO-string
 * slicing sidesteps the formatter entirely for minute precision.
 */
function fmtUtcMinute(d: Date): string {
  // "2026-04-17T10:00:00.000Z" → "2026-04-17 10:00 UTC"
  return `${d.toISOString().slice(0, 16).replace("T", " ")} UTC`;
}

function fmtMs(ms: number | null): string {
  if (ms == null) return "—";
  return `${ms.toFixed(ms < 10 ? 1 : 0)} ms`;
}

function fmtPct(pct: number | null, digits = 2): string {
  if (pct == null) return "—";
  return `${pct.toFixed(digits)}%`;
}

function fmtDeltaPct(delta: number | null): string {
  if (delta == null) return "—";
  const sign = delta >= 0 ? "+" : "";
  return `${sign}${delta.toFixed(0)}%`;
}

export default function Report() {
  const search = useSearch({ strict: false }) as ReportSearch;
  const { source_id, target_id, from, to, protocol } = search;

  const overview = usePathOverview({
    source: source_id,
    target: target_id,
    range: "custom",
    customFrom: from,
    customTo: to,
    protocol,
  });

  const snapshots = overview.data?.recent_snapshots ?? [];
  const primaryProto = overview.data?.primary_protocol ?? null;
  // The Report is scoped to a single protocol per its header. Backend
  // `recent_snapshots` is newest-first but unfiltered by protocol (see
  // `path_overview.rs`), so when the window contains mixed protocols a
  // naive first/last pick can pair AFTER and BEFORE from different
  // protocols — a meaningless cross-family diff. Restrict the candidate
  // pool to the primary protocol before selecting ids.
  const protocolSnapshots = primaryProto
    ? snapshots.filter((s) => s.protocol === primaryProto)
    : [];
  // AFTER is the newest protocol-matching entry; BEFORE is the earliest
  // distinct-id entry. When only one distinct id exists, BEFORE falls
  // back to AFTER and the summary flags `singleSnapshot` so operators
  // see there's no diff to compare.
  const afterId = protocolSnapshots[0]?.id;
  const beforeId = [...protocolSnapshots].reverse().find((s) => s.id !== afterId)?.id ?? afterId;

  const beforeQ = useRouteSnapshot({
    source: source_id,
    target: target_id,
    id: beforeId,
  });
  const afterQ = useRouteSnapshot({
    source: source_id,
    target: target_id,
    id: afterId,
  });

  const summary = useMemo(() => {
    if (!beforeQ.data || !afterQ.data) return null;
    const m = overview.data?.metrics;
    const first: MetricsPoint | null =
      m && m.rtt_series.length > 0
        ? {
            rtt_ms: m.rtt_series[0][1],
            loss: m.loss_series[0]?.[1] ?? null,
          }
        : null;
    const last: MetricsPoint | null =
      m && m.rtt_current != null ? { rtt_ms: m.rtt_current, loss: m.loss_current ?? null } : null;
    return buildReportSummary({
      before: beforeQ.data,
      after: afterQ.data,
      metricsFirst: first,
      metricsLast: last,
    });
  }, [beforeQ.data, afterQ.data, overview.data]);

  const routeDiff: RouteTableDiff | undefined = useMemo(() => {
    if (!beforeQ.data || !afterQ.data) return undefined;
    if (beforeQ.data.id === afterQ.data.id) return undefined;
    const d = computeRouteDiff(beforeQ.data.hops, afterQ.data.hops);
    const changedPositions = new Set<number>();
    const addedPositions = new Set<number>();
    const removedPositions = new Set<number>();
    for (const [pos, hop] of d.perHop) {
      switch (hop.kind) {
        case "ip_changed":
        case "latency_changed":
        case "both_changed":
          changedPositions.add(pos);
          break;
        case "added":
          addedPositions.add(pos);
          break;
        case "removed":
          removedPositions.add(pos);
          break;
      }
    }
    return { changedPositions, addedPositions, removedPositions };
  }, [beforeQ.data, afterQ.data]);

  if (overview.isLoading) {
    return (
      <div className="p-6">
        <Skeleton className="h-64 w-full" data-testid="report-skeleton" />
      </div>
    );
  }
  if (overview.isError || !overview.data) {
    return (
      <p role="alert" className="p-6 text-sm text-destructive">
        Failed to load report.
      </p>
    );
  }

  const data = overview.data;
  const primary = data.primary_protocol ?? null;
  const windowStart = new Date(data.window.from);
  const windowEnd = new Date(data.window.to);

  // On screen, `mx-auto max-w-4xl` keeps the report readable on wide
  // monitors. At print time those combine with Chrome's viewport-based
  // rendering to leave large empty margins around a narrow block — the
  // @page rule in globals.css sizes the sheet, `print:mx-0
  // print:max-w-none` lets the article fill it, and per-cell padding on
  // tables (see globals.css @media print) keeps columns compact.
  return (
    <article className="mx-auto max-w-4xl p-6 print:mx-0 print:max-w-none print:p-0">
      <header className="flex items-start justify-between gap-4 border-b pb-4 print:border-black">
        <div>
          <h1 className="text-xl font-semibold">Network Issue Report</h1>
          <p className="text-sm text-muted-foreground">Generated {fmtUtcMinute(new Date())}</p>
        </div>
        <Button type="button" onClick={() => window.print()} className="print:hidden">
          Export PDF
        </Button>
      </header>

      <section className="mt-4 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <span className="text-muted-foreground">From:</span>
        <span className="font-mono">{data.source.ip}</span>
        <span className="text-muted-foreground">To:</span>
        <span className="font-mono">{data.target.ip}</span>
        <span className="text-muted-foreground">Protocol:</span>
        <span className="font-semibold uppercase">{primary ?? "—"}</span>
        <span className="text-muted-foreground">Window:</span>
        <span>
          {fmtUtcMinute(windowStart)} – {fmtUtcMinute(windowEnd)}
        </span>
      </section>

      {primary == null ? (
        <section className="mt-6 rounded border border-dashed p-6 text-center text-sm text-muted-foreground">
          No data in window — try a wider range or a different protocol.
        </section>
      ) : (
        <>
          <section className="mt-6 print:break-inside-avoid">
            <h2 className="mb-2 text-lg font-semibold">Summary</h2>
            {summary ? (
              <ul className="list-disc pl-5 text-sm">
                <li>
                  RTT {fmtMs(summary.rttBeforeMs)} → {fmtMs(summary.rttAfterMs)} (
                  {fmtDeltaPct(summary.rttDeltaPct)})
                </li>
                <li>
                  Loss {fmtPct(summary.lossBeforePct)} → {fmtPct(summary.lossAfterPct)}
                </li>
                <li>Route {summary.routeChanged ? "changed" : "unchanged"} in window</li>
                {summary.singleSnapshot && (
                  <li className="text-muted-foreground">
                    Single snapshot in window — no before/after comparison.
                  </li>
                )}
              </ul>
            ) : (
              <p className="text-sm text-muted-foreground">Computing…</p>
            )}
          </section>

          {data.recent_snapshots_truncated && (
            <p className="mt-4 rounded border border-yellow-500/50 bg-yellow-500/10 p-2 text-sm print:border-black">
              Showing latest 100 snapshots — narrow the window for more.
            </p>
          )}

          <section className="mt-6 print:break-inside-avoid">
            <h2 className="mb-2 text-lg font-semibold">
              Route — BEFORE{" "}
              {beforeQ.data && (
                <span className="text-sm text-muted-foreground">
                  ({fmtUtcMinute(new Date(beforeQ.data.observed_at))})
                </span>
              )}
            </h2>
            {beforeQ.isLoading ? (
              <Skeleton className="h-24 w-full" />
            ) : beforeQ.isError ? (
              <p className="text-sm text-destructive">Failed to load BEFORE snapshot.</p>
            ) : beforeQ.data ? (
              <RouteTable hops={beforeQ.data.hops} />
            ) : (
              <p className="text-sm text-muted-foreground">No BEFORE snapshot available.</p>
            )}
          </section>

          <section className="mt-6 print:break-inside-avoid">
            <h2 className="mb-2 text-lg font-semibold">
              Route — AFTER{" "}
              {afterQ.data && (
                <span className="text-sm text-muted-foreground">
                  ({fmtUtcMinute(new Date(afterQ.data.observed_at))})
                </span>
              )}
            </h2>
            {afterQ.isLoading ? (
              <Skeleton className="h-24 w-full" />
            ) : afterQ.isError ? (
              <p className="text-sm text-destructive">Failed to load AFTER snapshot.</p>
            ) : afterQ.data ? (
              <RouteTable hops={afterQ.data.hops} diff={routeDiff} />
            ) : (
              <p className="text-sm text-muted-foreground">No AFTER snapshot available.</p>
            )}
          </section>

          <section className="mt-6 print:break-inside-avoid">
            <h2 className="mb-2 text-lg font-semibold">Measurement timeline</h2>
            {data.metrics == null ? (
              <p className="text-sm text-muted-foreground">Metrics unavailable.</p>
            ) : (
              <div className="grid gap-3 md:grid-cols-2">
                <GrafanaPanel
                  dashboard={MESHMON_PATH_DASHBOARD}
                  panelId={PANEL_RTT}
                  vars={{ source: source_id, target: target_id, protocol: primary }}
                  from={new Date(data.window.from).getTime().toString()}
                  to={new Date(data.window.to).getTime().toString()}
                  title="RTT"
                />
                <GrafanaPanel
                  dashboard={MESHMON_PATH_DASHBOARD}
                  panelId={PANEL_LOSS}
                  vars={{ source: source_id, target: target_id, protocol: primary }}
                  from={new Date(data.window.from).getTime().toString()}
                  to={new Date(data.window.to).getTime().toString()}
                  title="Loss"
                />
              </div>
            )}
          </section>

          <section className="mt-6 text-sm text-muted-foreground print:break-inside-avoid">
            <h2 className="mb-1 text-lg font-semibold text-foreground">Methodology</h2>
            <p>
              meshmon agents probe every peer with ICMP, UDP echo, and TCP connect continuously. RTT
              and loss are 60-second rolling averages of every probe; route snapshots are captured
              whenever the hop chain changes. Numbers in this report come from the agents named
              above and the window bounds in the header — no derived baselines.
            </p>
          </section>
        </>
      )}
    </article>
  );
}
