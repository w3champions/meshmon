import { formatDistanceToNowStrict } from "date-fns";
import { useMemo, useState } from "react";
import type { HistoryMeasurement } from "@/api/hooks/history";
import type { components } from "@/api/schema.gen";
import { RouteTopology } from "@/components/RouteTopology";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

type HopJson = components["schemas"]["HopJson"];

interface MtrTracesListProps {
  measurements: readonly HistoryMeasurement[];
  className?: string;
}

/**
 * One collapsible row per MTR trace — measurements with `mtr_hops != null`.
 *
 * Rows are sorted newest-first on `mtr_captured_at` (falling back to
 * `measured_at` when the capture timestamp is absent). Each row collapses
 * to `"<protocol> — <relative time>"`; expanding reveals the full
 * `RouteTopology` inline. `RouteTopology` receives the hops verbatim — no
 * transformation.
 */
export function MtrTracesList({ measurements, className }: MtrTracesListProps) {
  const traces = useMemo(() => {
    return measurements
      .filter(
        (m): m is HistoryMeasurement & { mtr_hops: HopJson[] } =>
          Array.isArray(m.mtr_hops) && m.mtr_hops.length > 0,
      )
      .slice()
      .sort((a, b) => {
        const aKey = a.mtr_captured_at ?? a.measured_at;
        const bKey = b.mtr_captured_at ?? b.measured_at;
        return bKey.localeCompare(aKey);
      });
  }, [measurements]);

  if (traces.length === 0) {
    return (
      <p role="status" className={cn("text-sm text-muted-foreground", className)}>
        No MTR traces in the selected window.
      </p>
    );
  }

  return (
    <ul className={cn("flex flex-col gap-2", className)} aria-label="MTR traces">
      {traces.map((m) => (
        <MtrTraceRow key={m.id} measurement={m} />
      ))}
    </ul>
  );
}

interface MtrTraceRowProps {
  measurement: HistoryMeasurement & { mtr_hops: HopJson[] };
}

function MtrTraceRow({ measurement }: MtrTraceRowProps) {
  const [expanded, setExpanded] = useState(false);
  const capturedAt = measurement.mtr_captured_at ?? measurement.measured_at;
  const relative = relativeOrUnknown(capturedAt);

  return (
    <li className="overflow-hidden rounded border bg-card/60">
      <Button
        type="button"
        variant="ghost"
        onClick={() => setExpanded((prev) => !prev)}
        aria-expanded={expanded}
        className="flex w-full items-center justify-between gap-4 rounded-none px-3 py-2 text-left"
      >
        <span className="flex items-center gap-3">
          <span className="rounded bg-secondary px-2 py-0.5 font-mono text-xs uppercase">
            {measurement.protocol}
          </span>
          <span className="text-sm" title={capturedAt}>
            {relative}
          </span>
          <span className="text-xs text-muted-foreground">
            {measurement.mtr_hops.length} hop{measurement.mtr_hops.length === 1 ? "" : "s"}
          </span>
        </span>
        <span aria-hidden className="text-xs text-muted-foreground">
          {expanded ? "Hide" : "Show"}
        </span>
      </Button>
      {expanded && (
        <div className="border-t p-3">
          <RouteTopology
            hops={measurement.mtr_hops}
            ariaLabel={`MTR trace captured at ${capturedAt}`}
            className="h-64 w-full"
          />
        </div>
      )}
    </li>
  );
}

function relativeOrUnknown(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "unknown time";
  return `${formatDistanceToNowStrict(d)} ago`;
}
