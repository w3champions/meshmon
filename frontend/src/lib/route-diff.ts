import type { components } from "@/api/schema.gen";

type HopJson = components["schemas"]["HopJson"];

export type HopChangeKind =
  | "unchanged"
  | "ip_changed"
  | "latency_changed"
  | "both_changed"
  | "added"
  | "removed";

export interface HopDiff {
  position: number;
  kind: HopChangeKind;
  aAvgRttMicros?: number;
  bAvgRttMicros?: number;
  aDominantIp?: string;
  bDominantIp?: string;
}

export interface DiffSummary {
  totalHops: number;
  changedHops: number;
  addedHops: number;
  removedHops: number;
  firstChangedPosition: number | null;
}

export interface RouteDiff {
  summary: DiffSummary;
  perHop: Map<number, HopDiff>;
}

const LATENCY_CHANGE_THRESHOLD = 0.5;

function dominantIp(hop: HopJson): string | undefined {
  if (hop.observed_ips.length === 0) return undefined;
  let best = hop.observed_ips[0];
  for (const ip of hop.observed_ips) {
    if (ip.freq > best.freq) best = ip;
  }
  return best.ip;
}

function byPosition(hops: HopJson[]): Map<number, HopJson> {
  const out = new Map<number, HopJson>();
  for (const h of hops) out.set(h.position, h);
  return out;
}

function latencyChanged(a: number, b: number): boolean {
  if (a <= 0) return b > 0;
  return Math.abs(b - a) / a >= LATENCY_CHANGE_THRESHOLD;
}

export function computeRouteDiff(a: HopJson[], b: HopJson[]): RouteDiff {
  const aByPos = byPosition(a);
  const bByPos = byPosition(b);
  const positions = new Set<number>([...aByPos.keys(), ...bByPos.keys()]);
  const perHop = new Map<number, HopDiff>();

  let changedHops = 0;
  let addedHops = 0;
  let removedHops = 0;
  let firstChangedPosition: number | null = null;

  for (const pos of [...positions].sort((x, y) => x - y)) {
    const ah = aByPos.get(pos);
    const bh = bByPos.get(pos);

    let kind: HopChangeKind;
    if (ah && !bh) {
      kind = "removed";
      removedHops += 1;
    } else if (!ah && bh) {
      kind = "added";
      addedHops += 1;
    } else if (ah && bh) {
      const ipDiff = dominantIp(ah) !== dominantIp(bh);
      const latDiff = latencyChanged(ah.avg_rtt_micros, bh.avg_rtt_micros);
      if (ipDiff && latDiff) kind = "both_changed";
      else if (ipDiff) kind = "ip_changed";
      else if (latDiff) kind = "latency_changed";
      else kind = "unchanged";
      if (kind !== "unchanged") changedHops += 1;
    } else {
      continue;
    }

    if (kind !== "unchanged" && firstChangedPosition === null) {
      firstChangedPosition = pos;
    }

    perHop.set(pos, {
      position: pos,
      kind,
      aAvgRttMicros: ah?.avg_rtt_micros,
      bAvgRttMicros: bh?.avg_rtt_micros,
      aDominantIp: ah ? dominantIp(ah) : undefined,
      bDominantIp: bh ? dominantIp(bh) : undefined,
    });
  }

  return {
    summary: {
      totalHops: positions.size,
      changedHops,
      addedHops,
      removedHops,
      firstChangedPosition,
    },
    perHop,
  };
}
