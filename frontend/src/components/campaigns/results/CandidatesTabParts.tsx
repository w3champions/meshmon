/**
 * Presentational sub-components for {@link CandidatesTab}. Kept in a
 * sibling file so the tab module itself stays under the 400-line ceiling
 * and the per-row / tab-level action UIs can evolve independently.
 */

import type { Campaign } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

// ---------------------------------------------------------------------------
// Row-level action menu (per candidate)
// ---------------------------------------------------------------------------

export interface RowActionPair {
  source_agent_id: string;
  destination_ip: string;
}

export interface RowActionMenuProps {
  candidate: Evaluation["results"]["candidates"][number];
  onForcePair: (pair: RowActionPair) => void;
  onTriggerPairDetail: (pair: RowActionPair) => void;
}

/**
 * Per-row dropdown with the "force re-measure" + "dispatch detail for
 * this pair" actions. The server-side pair row is keyed by
 * `(source_agent_id, destination_ip)`; when a candidate has multiple
 * scored pairs the drawer is the operator's lever for per-pair dispatch
 * and the row-level shortcut targets the first scored pair.
 */
export function RowActionMenu({ candidate, onForcePair, onTriggerPairDetail }: RowActionMenuProps) {
  const firstPair = candidate.pair_details[0];
  if (!firstPair) return null;

  const pair: RowActionPair = {
    source_agent_id: firstPair.source_agent_id,
    destination_ip: candidate.destination_ip,
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          aria-label={`Actions for ${candidate.destination_ip}`}
        >
          ⋯
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem onClick={() => onForcePair(pair)}>Force re-measure pair</DropdownMenuItem>
        <DropdownMenuItem onClick={() => onTriggerPairDetail(pair)}>
          Dispatch detail for this pair
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

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

// ---------------------------------------------------------------------------
// Tab-level overflow placeholder
// ---------------------------------------------------------------------------

export interface TabOverflowPlaceholderProps {
  campaign: Campaign;
}

/**
 * Placeholder trigger for the Task 18 `OverflowMenu`. Task 15 gates the
 * "Detail: good candidates only" item strictly on `campaign.state ===
 * "evaluated"` — a stale evaluation on a `completed` campaign should NOT
 * re-enable it. The placeholder exposes the gate state via a disabled
 * affordance so integration tests can assert on it today; Batch 5 swaps
 * the placeholder for the real dropdown with a cost-preview confirmation
 * dialog.
 */
export function TabOverflowPlaceholder({ campaign }: TabOverflowPlaceholderProps) {
  const goodCandidatesEnabled = campaign.state === "evaluated";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-label="Candidates tab actions"
          data-testid="candidates-overflow-trigger"
        >
          Actions
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem disabled data-testid="overflow-detail-all">
          Detail: all (Batch 5)
        </DropdownMenuItem>
        <DropdownMenuItem
          disabled={!goodCandidatesEnabled}
          data-testid="overflow-detail-good"
          aria-disabled={!goodCandidatesEnabled}
        >
          Detail: good candidates only (Batch 5)
        </DropdownMenuItem>
        <DropdownMenuItem disabled data-testid="overflow-re-evaluate">
          Re-evaluate (Batch 5)
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
