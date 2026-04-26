/**
 * Pure aggregation helpers for CompareTab client-side re-aggregation.
 *
 * For edge_candidate mode: given a full set of edge pair rows, re-aggregate
 * per-candidate metrics over a filtered agent subset.
 *
 * For diversity/optimization: candidate sub-picker + pick_role filtering stubs
 * (full wiring deferred — see Phase P followup comment in CompareTab.tsx).
 */

import type { EvaluationEdgePairDetailDto } from "@/api/hooks/evaluation";
import type { Evaluation } from "@/api/hooks/evaluation";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type Candidate = Evaluation["results"]["candidates"][number];

export interface CompareAggregate {
  destination_ip: string;
  coverage_count: number;
  total_picked: number;
  mean_ms_under_t: number | null;
  /** Route mix fractions (0–1), recomputed over filtered rows. */
  direct_share: number | null;
  onehop_share: number | null;
  twohop_share: number | null;
}

// ---------------------------------------------------------------------------
// Edge-candidate re-aggregation
// ---------------------------------------------------------------------------

/**
 * Re-aggregate edge-pair rows over a picked subset of destination agents.
 *
 * Returns one `CompareAggregate` per candidate IP found in `rows`, filtered
 * to `pickedAgentIds`. When `pickedAgentIds` is empty, returns an empty array
 * so the UI can prompt the user to pick at least one agent.
 */
export function aggregateEdgeCandidates(
  rows: EvaluationEdgePairDetailDto[],
  pickedAgentIds: ReadonlySet<string>,
): CompareAggregate[] {
  if (pickedAgentIds.size === 0) return [];

  // Group by candidate_ip over the picked-agent rows.
  const byCandidate = new Map<
    string,
    {
      qualifyingMs: number[];
      direct: number;
      oneHop: number;
      twoHop: number;
      totalPicked: number;
    }
  >();

  for (const row of rows) {
    if (!pickedAgentIds.has(row.destination_agent_id)) continue;

    let bucket = byCandidate.get(row.candidate_ip);
    if (!bucket) {
      bucket = { qualifyingMs: [], direct: 0, oneHop: 0, twoHop: 0, totalPicked: 0 };
      byCandidate.set(row.candidate_ip, bucket);
    }

    bucket.totalPicked++;

    // `best_route_ms` is `null` for unreachable rows; the `is_unreachable`
    // gate covers that case, but defend the type narrowing explicitly so
    // we never push `null` into `qualifyingMs`.
    if (!row.is_unreachable && row.qualifies_under_t && row.best_route_ms != null) {
      bucket.qualifyingMs.push(row.best_route_ms);
    }

    if (!row.is_unreachable) {
      switch (row.best_route_kind) {
        case "direct":
          bucket.direct++;
          break;
        case "one_hop":
          bucket.oneHop++;
          break;
        case "two_hop":
          bucket.twoHop++;
          break;
      }
    }
  }

  const aggregates: CompareAggregate[] = [];

  for (const [ip, bucket] of byCandidate) {
    const coverageCount = bucket.qualifyingMs.length;
    const meanMsUnderT =
      coverageCount > 0
        ? bucket.qualifyingMs.reduce((a, b) => a + b, 0) / coverageCount
        : null;

    const reachableTotal = bucket.direct + bucket.oneHop + bucket.twoHop;
    const direct_share = reachableTotal > 0 ? bucket.direct / reachableTotal : null;
    const onehop_share = reachableTotal > 0 ? bucket.oneHop / reachableTotal : null;
    const twohop_share = reachableTotal > 0 ? bucket.twoHop / reachableTotal : null;

    aggregates.push({
      destination_ip: ip,
      coverage_count: coverageCount,
      total_picked: bucket.totalPicked,
      mean_ms_under_t: meanMsUnderT,
      direct_share,
      onehop_share,
      twohop_share,
    });
  }

  return aggregates;
}

/**
 * Merge a `CompareAggregate` back onto the baseline `EvaluationCandidateDto`
 * shape so the existing `DrilldownDialog` / `EdgeCandidateTable` row rendering
 * receives a compatible object.
 *
 * Fields not recomputed here (e.g. `coverage_weighted_ping_ms`) are set to
 * `null` — the Compare table renders "—" for those.
 */
export function mergeAggregateIntoCandidate(
  baseline: Candidate,
  agg: CompareAggregate,
): Candidate {
  return {
    ...baseline,
    coverage_count: agg.coverage_count,
    mean_ms_under_t: agg.mean_ms_under_t,
    destinations_total: agg.total_picked,
    direct_share: agg.direct_share,
    onehop_share: agg.onehop_share,
    twohop_share: agg.twohop_share,
    // Coverage-weighted ping is intentionally deferred (formula depends on
    // full picked-pair count vs coverage; defer rather than compute wrong).
    coverage_weighted_ping_ms: null,
  };
}
