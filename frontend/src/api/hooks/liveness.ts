import { useSession } from "@/api/hooks/session";
import {
  DEFAULT_LIVENESS_THRESHOLDS,
  type LivenessThresholds,
  thresholdsFromConfig,
} from "@/lib/health";

/**
 * Read agent-liveness thresholds from `/api/session`, falling back to the
 * library defaults until the session response loads. The session query
 * has `staleTime: Infinity`, so the thresholds are read once per app
 * lifecycle and stay stable until a hard refresh.
 *
 * Thresholds are sourced from `[agents]` in the service config so an
 * operator override propagates without a frontend rebuild — see
 * `crates/service/src/http/session.rs::AgentLivenessConfig`.
 */
export function useAgentLivenessThresholds(): LivenessThresholds {
  const { data } = useSession();
  if (!data?.agents) {
    return DEFAULT_LIVENESS_THRESHOLDS;
  }
  return thresholdsFromConfig(data.agents);
}
