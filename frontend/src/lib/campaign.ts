/**
 * Shared helpers and display mappings for campaign pages.
 *
 * Campaign mutation hooks throw `new Error("failed to …", { cause: <body> })`
 * where `cause` is the openapi-fetch response body (e.g. `{error: "…"}` on
 * 4xx). Drill into `cause` defensively — other shapes fall through to the
 * generic error copy in the call site.
 */

import type { CampaignState } from "@/api/hooks/campaigns";

/**
 * Best-effort extractor for the server's `{error: "<code>"}` discriminator.
 * Returns `null` when the error isn't a structured API body.
 */
export function extractCampaignErrorCode(err: unknown): string | null {
  if (!(err instanceof Error)) return null;
  const cause: unknown = err.cause;
  if (cause === null || typeof cause !== "object") return null;
  const code = (cause as { error?: unknown }).error;
  return typeof code === "string" ? code : null;
}

/**
 * Narrow a mutation error to the 409 `illegal_state_transition` code the
 * server emits when a Start/Stop click lands on a campaign whose state has
 * already advanced (stale UI).
 */
export function isIllegalStateTransition(err: unknown): boolean {
  return extractCampaignErrorCode(err) === "illegal_state_transition";
}

/**
 * 409 from `POST /evaluate` when the campaign has no prior evaluation to
 * refresh. Surfaced as a neutral "nothing to re-evaluate" toast.
 */
export function isNoEvaluation(err: unknown): boolean {
  return extractCampaignErrorCode(err) === "no_evaluation";
}

/**
 * 400 from `POST /detail` when the requested scope resolves to zero pairs.
 * Drives the "nothing to remeasure" toast on the overflow menu.
 */
export function isNoPairsSelected(err: unknown): boolean {
  return extractCampaignErrorCode(err) === "no_pairs_selected";
}

/**
 * 400 from `POST /evaluate` when the campaign has no baseline pairs to score
 * against. Signals the operator the campaign has not actually run probes.
 */
export function isNoBaselinePairs(err: unknown): boolean {
  return extractCampaignErrorCode(err) === "no_baseline_pairs";
}

/**
 * 404 from `GET /evaluation` when the campaign has never been evaluated.
 * Paired with `useEvaluation`'s null-on-404 fallback — the matcher is
 * available for callers that want to branch on the raw response error.
 */
export function isNotEvaluated(err: unknown): boolean {
  return extractCampaignErrorCode(err) === "not_evaluated";
}

/**
 * 400 from `POST /detail` (scope = `pair`) when the destination IP does not
 * parse. Flags a malformed composer payload back to the user.
 */
export function isInvalidDestinationIp(err: unknown): boolean {
  return extractCampaignErrorCode(err) === "invalid_destination_ip";
}

/**
 * 404 from `POST /detail` (scope = `pair`) when the pair the request names
 * does not exist on the campaign. Defensive — should never fire in the
 * normal flow — but surfaced as a real toast if it does.
 */
export function isMissingPair(err: unknown): boolean {
  return extractCampaignErrorCode(err) === "missing_pair";
}

/**
 * 400 from `POST /detail` when the server could not parse the pair payload
 * at all. Signals a client/server shape drift bug.
 */
export function isUnexpectedPairPayload(err: unknown): boolean {
  return extractCampaignErrorCode(err) === "unexpected_pair_payload";
}

/**
 * 500 from `POST /evaluate` when the stored evaluation row fails to decode
 * back into the response DTO. Indicates a server-side bug; prompts the
 * operator to file an issue.
 */
export function isInvalidEvaluationPayload(err: unknown): boolean {
  return extractCampaignErrorCode(err) === "invalid_evaluation_payload";
}

/**
 * Map a lifecycle state to the shipped `Badge` variant. The primitive only
 * exposes `default | secondary | destructive | outline`, so terminal states
 * share `secondary` rather than inventing new variants.
 */
export function stateBadgeVariant(
  state: CampaignState,
): "default" | "secondary" | "destructive" | "outline" {
  switch (state) {
    case "draft":
      return "outline";
    case "running":
      return "default";
    case "completed":
    case "evaluated":
      return "secondary";
    case "stopped":
      return "destructive";
  }
}
