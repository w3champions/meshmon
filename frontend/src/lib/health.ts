export type HealthState = "normal" | "degraded" | "unreachable" | "stale";

/**
 * Agent-liveness verdict.
 *
 * - `online` — `last_seen_at` is fresh enough that the agent is reporting
 *   on its normal cadence.
 * - `stale` — `last_seen_at` could plausibly still be in flight from the
 *   server's registry refresh (snapshot may lag by `refresh_interval_seconds`).
 *   Renders as a soft warning rather than a hard offline badge.
 * - `offline` — past the configured `target_active_window_minutes`; pairs
 *   targeting this agent will be skipped.
 */
export type AgentLivenessState = "online" | "stale" | "offline";

/**
 * Defaults match the service config (`[agents]`):
 * - `target_active_window_minutes = 5`
 * - `refresh_interval_seconds = 10`
 *
 * The session response carries the live values so an operator override
 * propagates without a frontend rebuild. These constants only apply when
 * the session hasn't loaded yet.
 */
export const DEFAULT_OFFLINE_AFTER_MS = 5 * 60 * 1000;
export const DEFAULT_STALE_AFTER_MS = 2 * 10 * 1000;

/** Threshold inputs derived from the service's `[agents]` config. */
export interface LivenessThresholds {
  offlineAfterMs: number;
  staleAfterMs: number;
}

/** Project session-config minutes/seconds onto millisecond thresholds. */
export function thresholdsFromConfig(cfg: {
  target_active_window_minutes: number;
  refresh_interval_seconds: number;
}): LivenessThresholds {
  return {
    offlineAfterMs: cfg.target_active_window_minutes * 60_000,
    // 2× refresh interval — covers one missed refresh tick before flipping
    // to stale, which is the actual UX window that produced the flicker.
    staleAfterMs: 2 * cfg.refresh_interval_seconds * 1_000,
  };
}

export const DEFAULT_LIVENESS_THRESHOLDS: LivenessThresholds = {
  offlineAfterMs: DEFAULT_OFFLINE_AFTER_MS,
  staleAfterMs: DEFAULT_STALE_AFTER_MS,
};

export function classify(rate: number | undefined): HealthState {
  if (rate === undefined || Number.isNaN(rate)) return "stale";
  if (rate >= 0.2) return "unreachable";
  if (rate >= 0.05) return "degraded";
  return "normal";
}

/**
 * Compute an agent's liveness against the wall clock at call time.
 *
 * The comparison is `Date.now() - Date.parse(last_seen_at) > threshold`,
 * sampled fresh on every call. Callers MUST call this during render
 * (not memo-cached on a stale `now`) so a snapshot lag in the
 * `useAgents` query doesn't translate into a brief "offline" flicker
 * that persists past the next genuine push.
 *
 * Future timestamps (clock skew) are treated as `online`. Unparseable
 * strings are treated as `offline` — the API contract guarantees a
 * timestamp here, so a missing or malformed value is a real problem.
 */
export function getAgentLiveness(
  lastSeen: string,
  thresholds: LivenessThresholds = DEFAULT_LIVENESS_THRESHOLDS,
  now: number = Date.now(),
): AgentLivenessState {
  const t = Date.parse(lastSeen);
  if (!Number.isFinite(t)) return "offline";
  const age = now - t;
  if (age > thresholds.offlineAfterMs) return "offline";
  if (age > thresholds.staleAfterMs) return "stale";
  return "online";
}

/**
 * Binary "is this agent offline?" check, kept on the `isStale` name for
 * legacy call sites (the agents table, the agent card). Equivalent to
 * `getAgentLiveness(...) === "offline"`. New three-state badges should
 * call `getAgentLiveness` directly so they can render the soft "stale"
 * intermediate state.
 *
 * Future timestamps (clock skew) are treated as fresh, not stale.
 */
export function isStale(
  lastSeen: string,
  now: number = Date.now(),
  thresholds: LivenessThresholds = DEFAULT_LIVENESS_THRESHOLDS,
): boolean {
  return getAgentLiveness(lastSeen, thresholds, now) === "offline";
}
