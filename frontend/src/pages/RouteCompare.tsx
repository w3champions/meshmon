import { useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo } from "react";
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

  const midpointMs = ordered
    ? Math.round(
        (Date.parse(ordered.older.observed_at) + Date.parse(ordered.newer.observed_at)) / 2,
      )
    : Date.now();
  const protocol = ordered?.older.protocol ?? "icmp";

  const nearby: NearbySnapshotsResult = useNearbySnapshots({
    source,
    target,
    protocol,
    aroundTimeMs: midpointMs,
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
      // Ignore when typing in an input/textarea or inside the jump popover.
      const target = e.target as HTMLElement | null;
      if (target && /^(input|textarea|select)$/i.test(target.tagName)) return;

      const aNeighbors = nearby.getNeighbors(ordered.olderId);
      const bNeighbors = nearby.getNeighbors(ordered.newerId);
      switch (e.key) {
        case "j":
          if (aNeighbors.prev) onNavA(aNeighbors.prev);
          break;
        case "k":
          if (aNeighbors.next) onNavA(aNeighbors.next);
          break;
        case "l":
          if (bNeighbors.prev) onNavB(bNeighbors.prev);
          break;
        case ";":
          if (bNeighbors.next) onNavB(bNeighbors.next);
          break;
        case "g": {
          // Open the Jump popover for the last-focused card, defaulting to A
          // when no card subtree currently holds focus. Uses DOM data-attrs
          // instead of React refs so focus tracking stays stateless.
          const active = document.activeElement as HTMLElement | null;
          const card = active?.closest<HTMLElement>("[data-card-side]");
          const side = (card?.dataset.cardSide as "A" | "B" | undefined) ?? "A";
          const btn = document.querySelector<HTMLButtonElement>(`[data-jump-trigger="${side}"]`);
          btn?.click();
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
