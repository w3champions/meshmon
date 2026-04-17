import type { RouteSnapshotSummary } from "@/api/hooks/nearby-snapshots";
import { dayBoundariesBetween, formatShortDate, formatTickLabel } from "@/lib/time-format";
import { cn } from "@/lib/utils";

export interface TimeRailProps {
  side: "A" | "B";
  selectedId: number;
  selectedMs: number;
  snapshots: RouteSnapshotSummary[];
  otherMarkerMs: number;
  onTickClick(snapshot: RouteSnapshotSummary): void;
  maxTicks: number;
}

interface Tick {
  snapshot: RouteSnapshotSummary;
  timeMs: number;
  xFraction: number;
  blocked: boolean;
  blockedReason?: string;
  selected: boolean;
}

export function TimeRail({
  side,
  selectedId,
  selectedMs,
  snapshots,
  otherMarkerMs,
  onTickClick,
  maxTicks,
}: TimeRailProps) {
  if (snapshots.length === 0) {
    return <div className="text-xs text-muted-foreground">No nearby snapshots in the window.</div>;
  }

  const tickSet = subsampleTicks(snapshots, selectedId, maxTicks);
  const firstMs = Date.parse(tickSet[0].observed_at);
  const lastMs = Date.parse(tickSet[tickSet.length - 1].observed_at);
  const rangeSpan = Math.max(lastMs - firstMs, 1);
  const allTicksMs = tickSet.map((s) => Date.parse(s.observed_at));

  const ticks: Tick[] = tickSet.map((s) => {
    const timeMs = Date.parse(s.observed_at);
    const blocked = side === "A" ? timeMs >= otherMarkerMs : timeMs <= otherMarkerMs;
    return {
      snapshot: s,
      timeMs,
      xFraction: (timeMs - firstMs) / rangeSpan,
      blocked,
      blockedReason: blocked ? (side === "A" ? "crosses B" : "crosses A") : undefined,
      selected: s.id === selectedId,
    };
  });

  const boundaries = dayBoundariesBetween(firstMs, lastMs);

  return (
    <div className="relative h-14 rounded-md bg-muted/40 px-4">
      <div className="absolute inset-x-4 top-1/2 h-[2px] bg-muted-foreground/30" />

      {boundaries.map((bMs) => (
        <div
          key={bMs}
          className="absolute top-0 bottom-0 border-l border-dashed border-amber-500/70"
          style={{ left: `${pct((bMs - firstMs) / rangeSpan)}` }}
        >
          <span className="absolute -top-3 left-1 rounded bg-amber-500/15 px-1 font-mono text-[0.6rem] font-semibold text-amber-700">
            {formatShortDate(bMs)} →
          </span>
        </div>
      ))}

      {ticks.map((t) => {
        const label = formatTickLabel(t.timeMs, { allTicksMs, selectedMs }, t.selected);
        const accent = side === "A" ? "bg-indigo-500" : "bg-emerald-500";
        const sideSuffix = side === "A" ? "older" : "newer";
        const ariaLabel = t.blocked
          ? `${t.blockedReason}, at ${label} UTC`
          : `${label} UTC · ${sideSuffix} snapshot`;
        return (
          <button
            key={t.snapshot.id}
            type="button"
            aria-pressed={t.selected}
            disabled={t.blocked}
            title={t.blocked ? t.blockedReason : undefined}
            className={cn(
              "absolute top-1/2 flex h-7 w-7 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-full",
              t.blocked && "cursor-not-allowed opacity-35",
            )}
            style={{ left: `${pct(t.xFraction)}` }}
            onClick={() => !t.blocked && onTickClick(t.snapshot)}
            aria-label={ariaLabel}
          >
            <span
              className={cn(
                "h-2 w-2 rounded-full bg-muted-foreground/40",
                t.selected && `h-3.5 w-3.5 ${accent} ring-4 ring-offset-0 ring-black/5`,
              )}
            />
            <span
              className={cn(
                "absolute top-5 whitespace-nowrap font-mono text-[0.6rem]",
                t.selected
                  ? side === "A"
                    ? "font-bold text-indigo-600"
                    : "font-bold text-emerald-600"
                  : "text-muted-foreground",
                t.blocked && "text-muted-foreground/50",
              )}
            >
              {t.blocked ? (t.blockedReason ?? label) : label}
            </span>
          </button>
        );
      })}
    </div>
  );
}

function pct(fraction: number): string {
  const clamped = Math.max(0, Math.min(1, fraction));
  return `${(clamped * 100).toFixed(2)}%`;
}

function subsampleTicks(
  snapshots: RouteSnapshotSummary[],
  selectedId: number,
  maxTicks: number,
): RouteSnapshotSummary[] {
  if (snapshots.length <= maxTicks) return snapshots;
  const selectedIdx = snapshots.findIndex((s) => s.id === selectedId);
  const mandatory = new Set<number>([0, snapshots.length - 1]);
  if (selectedIdx >= 0) mandatory.add(selectedIdx);
  const remaining = maxTicks - mandatory.size;
  if (remaining > 0) {
    const step = (snapshots.length - 1) / (remaining + 1);
    for (let i = 1; i <= remaining; i += 1) mandatory.add(Math.round(step * i));
  }
  return [...mandatory]
    .sort((a, b) => a - b)
    .slice(0, maxTicks)
    .map((i) => snapshots[i]);
}
