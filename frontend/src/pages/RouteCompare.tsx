import { useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useRef } from "react";
import {
  type NearbySnapshotsResult,
  type RouteSnapshotSummary,
  useNearbySnapshots,
} from "@/api/hooks/nearby-snapshots";
import { useRouteSnapshot } from "@/api/hooks/route-snapshot";
import { RouteCompareHeader } from "@/components/RouteCompareHeader";
import { RouteDiffSummary } from "@/components/RouteDiffSummary";
import { RouteTable, type RouteTableDiff } from "@/components/RouteTable";
import { Skeleton } from "@/components/ui/skeleton";
import { computeRouteDiff } from "@/lib/route-diff";
import { formatClockUtcSec } from "@/lib/time-format";

export default function RouteCompare() {
  const { source, target } = useParams({ strict: false }) as {
    source: string;
    target: string;
  };
  const raw = useSearch({ strict: false }) as { a: number | string; b: number | string };
  const a = Number(raw.a);
  const b = Number(raw.b);
  const navigate = useNavigate();

  const snapA = useRouteSnapshot({ source, target, id: a });
  const snapB = useRouteSnapshot({ source, target, id: b });

  const bothLoaded = !!(snapA.data && snapB.data);
  const aMs = snapA.data ? Date.parse(snapA.data.observed_at) : 0;
  const bMs = snapB.data ? Date.parse(snapB.data.observed_at) : 0;

  // Chronological swap — display only. URL stays canonical.
  const ordered = useMemo(() => {
    if (!snapA.data || !snapB.data) return null;
    if (aMs <= bMs) return { older: snapA.data, newer: snapB.data, olderId: a, newerId: b };
    return { older: snapB.data, newer: snapA.data, olderId: b, newerId: a };
  }, [snapA.data, snapB.data, aMs, bMs, a, b]);

  // Gate the nearby query until both snapshots resolve so the hook never
  // fetches (or widens) around a placeholder `Date.now()` anchor. The
  // placeholder `midpointMs` below is stable across renders via the ref so
  // the queryKey doesn't churn even when the query is disabled.
  const fallbackMsRef = useRef<number>(Date.now());
  const midpointMs = ordered
    ? Math.round(
        (Date.parse(ordered.older.observed_at) + Date.parse(ordered.newer.observed_at)) / 2,
      )
    : fallbackMsRef.current;
  const protocol = ordered?.older.protocol ?? "icmp";

  const nearby: NearbySnapshotsResult = useNearbySnapshots({
    source,
    target,
    protocol,
    aroundTimeMs: midpointMs,
    enabled: !!ordered,
  });

  const navigateToPair = useCallback(
    (nextA: number, nextB: number) => {
      navigate({
        to: "/paths/$source/$target/routes/compare",
        params: { source, target },
        search: { a: nextA, b: nextB },
        replace: true,
      });
    },
    [navigate, source, target],
  );

  const onNavA = useCallback(
    (next: RouteSnapshotSummary) => {
      if (!ordered) return;
      navigateToPair(next.id, ordered.newerId);
    },
    [ordered, navigateToPair],
  );
  const onNavB = useCallback(
    (next: RouteSnapshotSummary) => {
      if (!ordered) return;
      navigateToPair(ordered.olderId, next.id);
    },
    [ordered, navigateToPair],
  );

  // Keyboard shortcuts: J/K step A, L/; step B, G opens jump popover.
  useEffect(() => {
    if (!ordered) return;
    const handler = (e: KeyboardEvent) => {
      // Ignore when any modifier is held so we don't intercept shortcuts like
      // Cmd+K (DevTools), Ctrl+L (URL bar focus), etc.
      if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
      const target = e.target as HTMLElement | null;
      if (target) {
        if (/^(input|textarea|select)$/i.test(target.tagName)) return;
        if (target.isContentEditable) return;
      }

      // Compare against the *ordered* (older/newer) timestamps so the guards
      // stay correct when the URL provides `a` and `b` in reverse
      // chronological order. A uses `ordered.older`; B uses `ordered.newer`.
      const olderMs = Date.parse(ordered.older.observed_at);
      const newerMs = Date.parse(ordered.newer.observed_at);
      const aNeighbors = nearby.getNeighbors(ordered.olderId);
      const bNeighbors = nearby.getNeighbors(ordered.newerId);
      // Mask step targets that would cross or equal the other marker so
      // keyboard shortcuts honour the same guard as the TimeRail ticks.
      const aNext =
        aNeighbors.next && Date.parse(aNeighbors.next.observed_at) < newerMs
          ? aNeighbors.next
          : undefined;
      const bPrev =
        bNeighbors.prev && Date.parse(bNeighbors.prev.observed_at) > olderMs
          ? bNeighbors.prev
          : undefined;
      switch (e.key) {
        case "j":
          if (aNeighbors.prev) onNavA(aNeighbors.prev);
          break;
        case "k":
          if (aNext) onNavA(aNext);
          break;
        case "l":
          if (bPrev) onNavB(bPrev);
          break;
        case ";":
          if (bNeighbors.next) onNavB(bNeighbors.next);
          break;
        case "g": {
          // Open the Jump popover for the last-focused card, defaulting to A
          // when no card subtree currently holds focus. Uses DOM data-attrs
          // instead of React refs so focus tracking stays stateless. The
          // responsive tiers all render simultaneously (display:none on
          // inactive tiers), so pick the first trigger whose offsetParent
          // is not null — i.e. the one the user can actually see.
          const active = document.activeElement as HTMLElement | null;
          const card = active?.closest<HTMLElement>("[data-card-side]");
          const side = (card?.dataset.cardSide as "A" | "B" | undefined) ?? "A";
          const candidates = document.querySelectorAll<HTMLButtonElement>(
            `[data-jump-trigger="${side}"]`,
          );
          const visible = Array.from(candidates).find((el) => el.offsetParent !== null);
          (visible ?? candidates[0])?.click();
          break;
        }
        default:
          return;
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [ordered, nearby, onNavA, onNavB]);

  if (snapA.isLoading || snapB.isLoading) {
    return <Skeleton className="h-64 w-full" data-testid="route-compare-skeleton" />;
  }

  if (!bothLoaded || !ordered) {
    return (
      <p role="alert" className="p-6 text-sm text-destructive">
        One of the snapshots could not be loaded.
      </p>
    );
  }

  const diff = computeRouteDiff(ordered.older.hops, ordered.newer.hops);
  const tableDiff: RouteTableDiff = {
    changedPositions: new Set(
      [...diff.perHop.values()]
        .filter(
          (h) =>
            h.kind === "ip_changed" || h.kind === "latency_changed" || h.kind === "both_changed",
        )
        .map((h) => h.position),
    ),
    addedPositions: new Set(
      [...diff.perHop.values()].filter((h) => h.kind === "added").map((h) => h.position),
    ),
    removedPositions: new Set(
      [...diff.perHop.values()].filter((h) => h.kind === "removed").map((h) => h.position),
    ),
  };

  return (
    <div className="flex flex-col gap-5 p-6">
      <RouteCompareHeader
        source={source}
        target={target}
        aDetail={ordered.older}
        bDetail={ordered.newer}
        nearby={nearby}
        onNavA={onNavA}
        onNavB={onNavB}
      />
      <RouteDiffSummary diff={diff} />
      <section className="flex flex-col gap-3">
        <div>
          <h2 className="mb-1 text-xs font-semibold uppercase tracking-wide text-indigo-700">
            A · {formatClockUtcSec(Date.parse(ordered.older.observed_at))}
          </h2>
          <RouteTable hops={ordered.older.hops} diff={tableDiff} />
        </div>
        <div>
          <h2 className="mb-1 text-xs font-semibold uppercase tracking-wide text-emerald-700">
            B · {formatClockUtcSec(Date.parse(ordered.newer.observed_at))}
          </h2>
          <RouteTable hops={ordered.newer.hops} diff={tableDiff} />
        </div>
      </section>
    </div>
  );
}
