import { useNavigate, useSearch } from "@tanstack/react-router";
import { useMemo } from "react";
import { useHistoryMeasurements } from "@/api/hooks/history";
import {
  HistoryPairFilters,
  type HistoryPairFiltersValue,
  type HistoryRange,
} from "@/components/history/HistoryPairFilters";
import { MtrTracesList } from "@/components/history/MtrTracesList";
import { PairChart } from "@/components/history/PairChart";
import { Skeleton } from "@/components/ui/skeleton";
import { type HistoryPairSearch, historyPairRoute } from "@/router/index";

/** Result cap enforced by `/api/history/measurements`. */
export const HISTORY_MEASUREMENTS_CAP = 5000;

/**
 * `/history/pair` — latency/loss + MTR history for one (source, destination)
 * pair. URL is the source of truth; picker interactions call
 * `navigate({ search, replace: true })` so shared URLs round-trip.
 *
 * The empty state before both picks are made is deliberately simple; once
 * source + destination are set, the page renders `<PairChart>` over the
 * measurements window plus an `<MtrTracesList>` for the MTR drilldown.
 */
export default function HistoryPair() {
  // `strict: false` keeps the hook usable under the component tests' ad-hoc
  // router tree (`HistoryPair.test.tsx` mounts the page under a throwaway
  // route whose id differs from production's `historyPairRoute.id`). The
  // router's `validateSearch` has already coerced the shape at the router
  // boundary via `historyPairSearchSchema`, so casting here is safe.
  const search = useSearch({ strict: false }) as HistoryPairSearch;
  const navigate = useNavigate({ from: historyPairRoute.fullPath });

  const filtersValue = useMemo<HistoryPairFiltersValue>(
    () => ({
      source: search.source,
      destination: search.destination,
      protocols: search.protocol ?? [],
      range: search.range,
      from: search.from,
      to: search.to,
    }),
    [search.source, search.destination, search.protocol, search.range, search.from, search.to],
  );

  const resolvedWindow = useMemo(
    () => resolveWindow(search.range, search.from, search.to),
    [search.range, search.from, search.to],
  );

  const measurementsFilter =
    search.source && search.destination
      ? {
          source: search.source,
          destination: search.destination,
          protocols: search.protocol && search.protocol.length > 0 ? search.protocol : undefined,
          from: resolvedWindow.from,
          to: resolvedWindow.to,
        }
      : null;

  const measurements = useHistoryMeasurements(measurementsFilter);

  const handleFiltersChange = (next: HistoryPairFiltersValue): void => {
    const nextSearch: HistoryPairSearch = {
      source: next.source,
      destination: next.destination,
      protocol: next.protocols.length > 0 ? [...next.protocols] : undefined,
      range: next.range,
      from: next.from,
      to: next.to,
    };
    // `useNavigate({ from: historyPairRoute.fullPath })` above narrows
    // `to` + `search` to this route's schema, so no casts are needed
    // when the component mounts under the production router. The
    // component-test harness in `HistoryPair.test.tsx` registers a
    // throwaway route at the same `/history/pair` path, which satisfies
    // the router-inferred `to` check at runtime.
    navigate({ to: "/history/pair", search: nextSearch, replace: true });
  };

  // Truncation probe: the backend asks for `HISTORY_MEASUREMENTS_CAP + 1`
  // and we display the first `HISTORY_MEASUREMENTS_CAP`. Receiving the
  // extra row (rowCount > cap) means the underlying set is larger than the
  // cap — i.e., the operator is seeing a truncated view. A response of
  // exactly `cap` rows means the backend returned the full set with no
  // truncation, so the cap notice stays hidden (mirrors the
  // `pair_counts`-driven Clone truncation check).
  const rawRows = measurements.data ?? [];
  const atCap = rawRows.length > HISTORY_MEASUREMENTS_CAP;
  const visibleRows = atCap ? rawRows.slice(0, HISTORY_MEASUREMENTS_CAP) : rawRows;

  return (
    <div className="flex flex-col">
      <HistoryPairFilters value={filtersValue} onChange={handleFiltersChange} />

      <div className="flex flex-col gap-6 p-4">
        {!search.source ? (
          <EmptyState message="Pick a source to begin." />
        ) : !search.destination ? (
          <EmptyState message="Pick a destination to see latency, loss, and MTR over time." />
        ) : measurements.isPending ? (
          <Skeleton className="h-96 w-full" data-testid="history-pair-skeleton" />
        ) : measurements.isError ? (
          <p role="alert" className="text-sm text-destructive">
            Failed to load history: {measurements.error.message}
          </p>
        ) : (
          <>
            {atCap && (
              <p
                role="status"
                aria-live="polite"
                className="rounded border border-yellow-500/40 bg-yellow-500/10 px-3 py-2 text-sm"
                data-testid="history-pair-cap-notice"
              >
                Showing most recent {HISTORY_MEASUREMENTS_CAP.toLocaleString()} measurements — the
                window exceeds the server cap. Narrow the time range or protocol filter to see older
                rows.
              </p>
            )}
            <section aria-labelledby="pair-chart-heading" className="flex flex-col gap-3">
              <h2 id="pair-chart-heading" className="text-lg font-semibold">
                Latency &amp; loss
              </h2>
              <PairChart measurements={visibleRows} />
            </section>
            <section aria-labelledby="pair-mtr-heading" className="flex flex-col gap-3">
              <h2 id="pair-mtr-heading" className="text-lg font-semibold">
                MTR traces
              </h2>
              <MtrTracesList measurements={visibleRows} />
            </section>
          </>
        )}
      </div>
    </div>
  );
}

interface EmptyStateProps {
  message: string;
}

function EmptyState({ message }: EmptyStateProps) {
  return (
    <div
      role="status"
      className="rounded border border-dashed p-12 text-center text-sm text-muted-foreground"
    >
      {message}
    </div>
  );
}

/**
 * Translate the filter-bar range into concrete `from` / `to` ISO strings
 * for the measurements query. Preset ranges anchor to "now"; custom ranges
 * pass through the operator-supplied bounds verbatim.
 */
function resolveWindow(
  range: HistoryRange,
  from: string | undefined,
  to: string | undefined,
): { from: string | undefined; to: string | undefined } {
  if (range === "custom") {
    return { from, to };
  }
  const millis: Record<Exclude<HistoryRange, "custom">, number> = {
    "24h": 24 * 3_600_000,
    "7d": 7 * 24 * 3_600_000,
    "30d": 30 * 24 * 3_600_000,
    "90d": 90 * 24 * 3_600_000,
  };
  const now = Date.now();
  return {
    from: new Date(now - millis[range]).toISOString(),
    to: new Date(now).toISOString(),
  };
}
