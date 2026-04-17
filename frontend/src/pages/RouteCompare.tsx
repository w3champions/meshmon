import { useParams, useSearch } from "@tanstack/react-router";
import { useRouteSnapshot } from "@/api/hooks/route-snapshot";
import { RouteDiffSummary } from "@/components/RouteDiffSummary";
import { RouteTopology } from "@/components/RouteTopology";
import { Skeleton } from "@/components/ui/skeleton";
import { computeRouteDiff } from "@/lib/route-diff";

export default function RouteCompare() {
  // `strict: false`: the production router mounts this under the `auth-guard`
  // id, but component tests wire it directly under a root route — so the
  // runtime route id differs. Zod `validateSearch` on `routeCompareRoute`
  // still runs in production, so the casts below are safe.
  const { source, target } = useParams({ strict: false }) as { source: string; target: string };
  const { a, b } = useSearch({ strict: false }) as { a: number; b: number };

  const snapA = useRouteSnapshot({ source, target, id: a });
  const snapB = useRouteSnapshot({ source, target, id: b });

  if (snapA.isLoading || snapB.isLoading) {
    return <Skeleton className="h-64 w-full" data-testid="route-compare-skeleton" />;
  }

  // `useRouteSnapshot` returns `null` on 404. After the isLoading gate above,
  // `data` being `null` or `undefined` means the snapshot couldn't be loaded.
  if (!snapA.data || !snapB.data) {
    return (
      <p role="alert" className="p-6 text-sm text-destructive">
        One of the snapshots could not be loaded.
      </p>
    );
  }

  const diff = computeRouteDiff(snapA.data.hops, snapB.data.hops);

  return (
    <div className="p-6 flex flex-col gap-6">
      <header className="text-sm text-muted-foreground">
        Comparing snapshots <span className="font-mono">{a}</span> and{" "}
        <span className="font-mono">{b}</span> for {source} → {target}.
      </header>
      <RouteDiffSummary diff={diff} />
      <section className="grid gap-3 md:grid-cols-2">
        <div>
          <h2 className="mb-2 text-sm font-semibold">A (#{a})</h2>
          <RouteTopology
            hops={snapA.data.hops}
            highlightChanges={diff.perHop}
            ariaLabel={`Route snapshot ${a}`}
          />
        </div>
        <div>
          <h2 className="mb-2 text-sm font-semibold">B (#{b})</h2>
          <RouteTopology
            hops={snapB.data.hops}
            highlightChanges={diff.perHop}
            ariaLabel={`Route snapshot ${b}`}
          />
        </div>
      </section>
    </div>
  );
}
