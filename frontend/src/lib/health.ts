export type HealthState = "normal" | "degraded" | "unreachable" | "stale";

// Matches the AgentOffline alert window in spec 05 / prometheus alert rules.
const STALE_MS = 5 * 60 * 1000;

export function classify(rate: number | undefined): HealthState {
  if (rate === undefined || Number.isNaN(rate)) return "stale";
  if (rate >= 0.2) return "unreachable";
  if (rate >= 0.05) return "degraded";
  return "normal";
}

// Future timestamps (clock skew) are treated as fresh, not stale.
export function isStale(lastSeen: string, now: number = Date.now()): boolean {
  const t = Date.parse(lastSeen);
  if (!Number.isFinite(t)) return true;
  return now - t > STALE_MS;
}
