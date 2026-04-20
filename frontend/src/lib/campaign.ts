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
