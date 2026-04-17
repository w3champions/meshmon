import type { RouteSnapshotSummary } from "@/api/hooks/nearby-snapshots";
import { Button } from "@/components/ui/button";
import { formatDelta } from "@/lib/time-format";

export interface TimeStepperProps {
  side: "A" | "B";
  selectedMs: number;
  prev?: RouteSnapshotSummary;
  next?: RouteSnapshotSummary;
  onStep(snapshot: RouteSnapshotSummary): void;
}

export function TimeStepper({ side, selectedMs, prev, next, onStep }: TimeStepperProps) {
  const prevDelta = prev ? selectedMs - Date.parse(prev.observed_at) : undefined;
  const nextDelta = next ? Date.parse(next.observed_at) - selectedMs : undefined;

  return (
    <div className="grid grid-cols-2 gap-0 border-t border-border">
      <Button
        type="button"
        variant="ghost"
        className="rounded-none font-mono text-xs"
        aria-label={
          prev
            ? `step ${side} ${formatDelta(prevDelta ?? 0)} earlier`
            : `no earlier snapshot for ${side}`
        }
        disabled={!prev}
        onClick={() => prev && onStep(prev)}
      >
        {prev ? (
          <>
            <span aria-hidden>◀</span>&nbsp;{formatDelta(prevDelta ?? 0)} earlier
          </>
        ) : (
          <span className="text-muted-foreground/60">— no earlier</span>
        )}
      </Button>
      <Button
        type="button"
        variant="ghost"
        className="rounded-none border-l border-border font-mono text-xs"
        aria-label={
          next
            ? `step ${side} ${formatDelta(nextDelta ?? 0)} later`
            : `no later snapshot for ${side}`
        }
        disabled={!next}
        onClick={() => next && onStep(next)}
      >
        {next ? (
          <>
            {formatDelta(nextDelta ?? 0)} later&nbsp;<span aria-hidden>▶</span>
          </>
        ) : (
          <span className="text-muted-foreground/60">no later —</span>
        )}
      </Button>
    </div>
  );
}
