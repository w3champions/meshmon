/**
 * Presentational sub-components for {@link CandidatesTab}. Kept in a
 * sibling file so the tab module itself stays under the 400-line ceiling.
 *
 * Per-pair operator actions (force re-measure, dispatch detail for a
 * single pair) live inside the drilldown dialog instead of on every
 * candidate row — the action requires a `(source_agent_id,
 * destination_ip)` tuple, which is reachable only from the paginated
 * `…/candidates/{ip}/pair_details` endpoint that the dialog already
 * fetches.
 */

import { Card } from "@/components/ui/card";

// ---------------------------------------------------------------------------
// Unqualified-reasons card
// ---------------------------------------------------------------------------

export interface UnqualifiedReasonsProps {
  reasons: Record<string, string>;
}

export function UnqualifiedReasons({ reasons }: UnqualifiedReasonsProps) {
  const entries = Object.entries(reasons);
  if (entries.length === 0) return null;
  return (
    <Card className="flex flex-col gap-2 p-4">
      <h3 className="text-sm font-semibold">Unqualified candidates</h3>
      <ul className="flex flex-col gap-1 text-sm text-muted-foreground">
        {entries.map(([ip, reason]) => (
          <li key={ip}>
            <span className="font-mono text-xs">{ip}</span> — {reason}
          </li>
        ))}
      </ul>
    </Card>
  );
}
