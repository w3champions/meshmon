import type { RouteDiff } from "@/lib/route-diff";

interface RouteDiffSummaryProps {
  diff: RouteDiff;
}

export function RouteDiffSummary({ diff }: RouteDiffSummaryProps) {
  const { summary } = diff;
  if (summary.changedHops === 0 && summary.addedHops === 0 && summary.removedHops === 0) {
    return <p className="text-sm text-muted-foreground">No changes between snapshots.</p>;
  }
  return (
    <ul className="text-sm flex flex-col gap-1">
      <li>{summary.totalHops} hops compared</li>
      <li>{summary.changedHops} changed</li>
      {summary.addedHops > 0 && <li>{summary.addedHops} added</li>}
      {summary.removedHops > 0 && <li>{summary.removedHops} removed</li>}
      {summary.firstChangedPosition !== null && (
        <li>First change at hop {summary.firstChangedPosition}</li>
      )}
    </ul>
  );
}
