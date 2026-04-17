import { useCallback, useMemo, useState } from "react";
import type { NearbySnapshotsResult, RouteSnapshotSummary } from "@/api/hooks/nearby-snapshots";
import type { components } from "@/api/schema.gen";
import { TimeJumpPopover } from "@/components/TimeJumpPopover";
import { TimeRail } from "@/components/TimeRail";
import { TimeStepper } from "@/components/TimeStepper";
import { Button } from "@/components/ui/button";
import {
  formatClockUtcSec,
  formatDelta,
  formatRelativeAgo,
  formatShortDate,
} from "@/lib/time-format";
import { cn } from "@/lib/utils";

export type RouteSnapshotDetail = components["schemas"]["RouteSnapshotDetail"];

export interface RouteCompareHeaderProps {
  source: string;
  target: string;
  aDetail: RouteSnapshotDetail;
  bDetail: RouteSnapshotDetail;
  nearby: NearbySnapshotsResult;
  onNavA(snapshot: RouteSnapshotSummary): void;
  onNavB(snapshot: RouteSnapshotSummary): void;
}

export function RouteCompareHeader({
  source,
  target,
  aDetail,
  bDetail,
  nearby,
  onNavA,
  onNavB,
}: RouteCompareHeaderProps) {
  const aMs = Date.parse(aDetail.observed_at);
  const bMs = Date.parse(bDetail.observed_at);
  const nowMs = Date.now();
  const deltaLabel = formatDelta(bMs - aMs);
  const aNeighbors = nearby.getNeighbors(aDetail.id);
  const bNeighbors = nearby.getNeighbors(bDetail.id);

  // Protocol should be consistent across A and B; guard with a sensible
  // fallback so snapshot pairs with drifted protocol don't crash the header.
  const protocol = (aDetail.protocol || bDetail.protocol || "").toUpperCase();

  const [copied, setCopied] = useState(false);
  const copyLink = () => {
    if (typeof window === "undefined" || !navigator.clipboard) return;
    navigator.clipboard
      .writeText(window.location.href)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 2000);
      })
      .catch(() => {
        /* clipboard failures are non-fatal */
      });
  };

  const handleJumpA = useCallback(
    (target: number) => {
      const snap = nearby.findClosest(target);
      if (snap) onNavA(snap);
    },
    [nearby, onNavA],
  );
  const handleJumpB = useCallback(
    (target: number) => {
      const snap = nearby.findClosest(target);
      if (snap) onNavB(snap);
    },
    [nearby, onNavB],
  );

  const aCard = useMemo(
    () => ({
      label: "A · older",
      accent: "indigo" as const,
      detail: aDetail,
      timeMs: aMs,
      relative: formatRelativeAgo(aMs, nowMs),
      prev: aNeighbors.prev,
      next: aNeighbors.next,
      onStep: onNavA,
      onJump: handleJumpA,
      onTickClick: onNavA,
      side: "A" as const,
    }),
    [aDetail, aMs, nowMs, aNeighbors.prev, aNeighbors.next, onNavA, handleJumpA],
  );

  const bCard = useMemo(
    () => ({
      label: "B · newer",
      accent: "emerald" as const,
      detail: bDetail,
      timeMs: bMs,
      relative: formatRelativeAgo(bMs, nowMs),
      prev: bNeighbors.prev,
      next: bNeighbors.next,
      onStep: onNavB,
      onJump: handleJumpB,
      onTickClick: onNavB,
      side: "B" as const,
    }),
    [bDetail, bMs, nowMs, bNeighbors.prev, bNeighbors.next, onNavB, handleJumpB],
  );

  return (
    <section className="flex flex-col gap-2">
      <div className="flex flex-wrap items-baseline gap-2 text-sm">
        <span className="font-mono font-bold">
          {source} → {target}
        </span>
        {protocol && (
          <span className="rounded bg-indigo-500/15 px-1.5 py-0.5 text-[0.65rem] font-semibold uppercase tracking-wide text-indigo-700">
            {protocol}
          </span>
        )}
        <span className="rounded-full bg-muted px-2 py-0.5 font-mono text-xs text-muted-foreground">
          Δ A→B · {deltaLabel}
        </span>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={copyLink}
          className="ml-auto h-6 text-xs"
          aria-live="polite"
        >
          {copied ? "✓ copied" : "⎘ copy link"}
        </Button>
      </div>

      {/* Tier 1 — side-by-side rails, ≥ 1024px. */}
      <div className="hidden lg:grid lg:grid-cols-2 lg:gap-3">
        {[aCard, bCard].map((card) => (
          <RailCard
            key={card.side}
            card={card}
            otherMarkerMs={card.side === "A" ? bMs : aMs}
            nearby={nearby}
            tierMaxTicks={7}
          />
        ))}
      </div>

      {/* Tier 2 — stacked rails, 640–1024px. */}
      <div className="hidden sm:flex sm:flex-col sm:gap-3 lg:hidden">
        {[aCard, bCard].map((card) => (
          <RailCard
            key={card.side}
            card={card}
            otherMarkerMs={card.side === "A" ? bMs : aMs}
            nearby={nearby}
            tierMaxTicks={7}
          />
        ))}
      </div>

      {/* Tier 3 — pill steppers, < 640px. */}
      <div className="flex flex-col gap-2 sm:hidden">
        {[aCard, bCard].map((card) => (
          <PillCard key={card.side} card={card} otherMarkerMs={card.side === "A" ? bMs : aMs} />
        ))}
      </div>
    </section>
  );
}

interface RailCardData {
  label: string;
  accent: "indigo" | "emerald";
  detail: RouteSnapshotDetail;
  timeMs: number;
  relative: string;
  prev?: RouteSnapshotSummary;
  next?: RouteSnapshotSummary;
  onStep(snapshot: RouteSnapshotSummary): void;
  onJump(targetMs: number): void;
  onTickClick(snapshot: RouteSnapshotSummary): void;
  side: "A" | "B";
}

function RailCard({
  card,
  otherMarkerMs,
  nearby,
  tierMaxTicks,
}: {
  card: RailCardData;
  otherMarkerMs: number;
  nearby: NearbySnapshotsResult;
  tierMaxTicks: number;
}) {
  const accentBorder =
    card.accent === "indigo"
      ? "border-l-[3px] border-l-indigo-500"
      : "border-l-[3px] border-l-emerald-500";
  const labelColor = card.accent === "indigo" ? "text-indigo-700" : "text-emerald-700";

  return (
    <div
      data-card-side={card.side}
      tabIndex={-1}
      className={cn("rounded-lg border bg-muted/30 p-3", accentBorder)}
    >
      <div className="mb-2 flex items-baseline justify-between gap-2">
        <div className="flex items-baseline gap-2">
          <span className={cn("text-[0.6rem] font-semibold uppercase tracking-wide", labelColor)}>
            {card.label}
          </span>
          <span className="font-mono text-sm font-bold">{formatClockUtcSec(card.timeMs)}</span>
          <span className="font-mono text-[0.65rem] text-muted-foreground">
            {formatShortDate(card.timeMs)}
          </span>
          <span className="font-mono text-[0.65rem] text-muted-foreground">· {card.relative}</span>
        </div>
        <TimeJumpPopover
          anchorTimeMs={card.timeMs}
          otherMarkerMs={otherMarkerMs}
          side={card.side}
          onRequestJump={card.onJump}
        >
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="h-6 text-[0.6rem] uppercase"
            data-jump-trigger={card.side}
          >
            Jump…
          </Button>
        </TimeJumpPopover>
      </div>
      <TimeRail
        side={card.side}
        selectedId={card.detail.id}
        selectedMs={card.timeMs}
        snapshots={nearby.snapshots}
        otherMarkerMs={otherMarkerMs}
        maxTicks={tierMaxTicks}
        onTickClick={card.onTickClick}
      />
    </div>
  );
}

function PillCard({ card, otherMarkerMs }: { card: RailCardData; otherMarkerMs: number }) {
  const accentBorder =
    card.accent === "indigo"
      ? "border-l-[3px] border-l-indigo-500"
      : "border-l-[3px] border-l-emerald-500";
  const labelColor = card.accent === "indigo" ? "text-indigo-700" : "text-emerald-700";
  return (
    <div
      data-card-side={card.side}
      tabIndex={-1}
      className={cn("overflow-hidden rounded-lg border bg-muted/30", accentBorder)}
    >
      <div className="flex items-baseline justify-between px-2 pt-2">
        <span className={cn("text-[0.58rem] font-semibold uppercase tracking-wide", labelColor)}>
          {card.label}
        </span>
        <span className="font-mono text-[0.62rem] text-muted-foreground">{card.relative}</span>
      </div>
      <div className="px-2 font-mono text-sm font-bold">{formatClockUtcSec(card.timeMs)}</div>
      <div className="px-2 pb-1 font-mono text-[0.6rem] text-muted-foreground">
        {formatShortDate(card.timeMs)}
      </div>
      <TimeStepper
        side={card.side}
        selectedMs={card.timeMs}
        prev={card.prev}
        next={card.next}
        onStep={card.onStep}
      />
      <TimeJumpPopover
        anchorTimeMs={card.timeMs}
        otherMarkerMs={otherMarkerMs}
        side={card.side}
        onRequestJump={card.onJump}
      >
        <button
          type="button"
          data-jump-trigger={card.side}
          className="w-full border-t border-dashed border-muted-foreground/25 bg-transparent py-1 text-[0.6rem] font-semibold uppercase tracking-wide text-muted-foreground"
        >
          Jump…
        </button>
      </TimeJumpPopover>
    </div>
  );
}
