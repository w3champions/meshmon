import type { components } from "@/api/schema.gen";

type HopJson = components["schemas"]["HopJson"];

export interface HopChange {
  position: number;
  from: HopJson;
  to: HopJson;
}

export interface RouteDiff {
  added: HopJson[];
  removed: HopJson[];
  changed: HopChange[];
  unchanged: HopJson[];
}

// Thresholds for "meaningfully different" at a hop, matching the path-detail
// spec: >20% RTT delta or >5-percentage-point loss change.
const RTT_CHANGE_THRESHOLD = 0.2;
const LOSS_CHANGE_THRESHOLD = 0.05;

/**
 * Pick the most-observed IP at a hop (highest `freq`). Returns `undefined`
 * for a hop with no observations so callers can decide how to render.
 */
function dominantIp(hop: HopJson): string | undefined {
  if (hop.observed_ips.length === 0) return undefined;
  let best = hop.observed_ips[0];
  for (const ip of hop.observed_ips) {
    if (ip.freq > best.freq) best = ip;
  }
  return best.ip;
}

function rttChanged(a: HopJson, b: HopJson): boolean {
  if (a.avg_rtt_micros === 0 && b.avg_rtt_micros === 0) return false;
  const base = Math.max(a.avg_rtt_micros, 1);
  return Math.abs(a.avg_rtt_micros - b.avg_rtt_micros) / base > RTT_CHANGE_THRESHOLD;
}

function lossChanged(a: HopJson, b: HopJson): boolean {
  return Math.abs(a.loss_pct - b.loss_pct) > LOSS_CHANGE_THRESHOLD;
}

function hopsAreDifferent(a: HopJson, b: HopJson): boolean {
  if (dominantIp(a) !== dominantIp(b)) return true;
  if (rttChanged(a, b)) return true;
  if (lossChanged(a, b)) return true;
  return false;
}

/**
 * Diff two ordered hop lists keyed by `position`. The result partitions hops
 * into added / removed / changed / unchanged buckets so the UI can render a
 * compact "what changed" summary.
 */
export function diffRouteSnapshots(a: HopJson[], b: HopJson[]): RouteDiff {
  const byPosA = new Map<number, HopJson>();
  for (const hop of a) byPosA.set(hop.position, hop);
  const byPosB = new Map<number, HopJson>();
  for (const hop of b) byPosB.set(hop.position, hop);

  const added: HopJson[] = [];
  const removed: HopJson[] = [];
  const changed: HopChange[] = [];
  const unchanged: HopJson[] = [];

  const positions = new Set<number>([...byPosA.keys(), ...byPosB.keys()]);
  const sorted = Array.from(positions).sort((x, y) => x - y);

  for (const pos of sorted) {
    const hopA = byPosA.get(pos);
    const hopB = byPosB.get(pos);
    if (hopA && !hopB) {
      removed.push(hopA);
    } else if (!hopA && hopB) {
      added.push(hopB);
    } else if (hopA && hopB) {
      if (hopsAreDifferent(hopA, hopB)) {
        changed.push({ position: pos, from: hopA, to: hopB });
      } else {
        unchanged.push(hopB);
      }
    }
  }

  return { added, removed, changed, unchanged };
}
