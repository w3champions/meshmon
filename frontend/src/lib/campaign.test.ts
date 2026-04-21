import { describe, expect, test } from "vitest";
import {
  extractCampaignErrorCode,
  isIllegalStateTransition,
  isInvalidDestinationIp,
  isInvalidEvaluationPayload,
  isMissingPair,
  isNoBaselinePairs,
  isNoEvaluation,
  isNoPairsSelected,
  isNotEvaluated,
  isUnexpectedPairPayload,
  stateBadgeVariant,
} from "@/lib/campaign";

function apiError(code: string): Error {
  // Matches the shape openapi-fetch lands on `Error.cause` for 4xx/5xx
  // responses: the parsed `{error: "…"}` body.
  return new Error("failed", { cause: { error: code } });
}

describe("extractCampaignErrorCode", () => {
  test("returns the code from a structured API body", () => {
    expect(extractCampaignErrorCode(apiError("illegal_state_transition"))).toBe(
      "illegal_state_transition",
    );
  });

  test("returns null for non-Error values", () => {
    expect(extractCampaignErrorCode("illegal_state_transition")).toBeNull();
    expect(extractCampaignErrorCode(null)).toBeNull();
    expect(extractCampaignErrorCode(undefined)).toBeNull();
  });

  test("returns null when the cause is not an object", () => {
    expect(extractCampaignErrorCode(new Error("boom"))).toBeNull();
    expect(extractCampaignErrorCode(new Error("boom", { cause: "nope" }))).toBeNull();
  });

  test("returns null when the cause has no string `error`", () => {
    expect(extractCampaignErrorCode(new Error("boom", { cause: { error: 42 } }))).toBeNull();
    expect(extractCampaignErrorCode(new Error("boom", { cause: {} }))).toBeNull();
  });
});

describe("error matchers", () => {
  test.each([
    ["isIllegalStateTransition", isIllegalStateTransition, "illegal_state_transition"],
    ["isNoEvaluation", isNoEvaluation, "no_evaluation"],
    ["isNoPairsSelected", isNoPairsSelected, "no_pairs_selected"],
    ["isNoBaselinePairs", isNoBaselinePairs, "no_baseline_pairs"],
    ["isNotEvaluated", isNotEvaluated, "not_evaluated"],
    ["isInvalidDestinationIp", isInvalidDestinationIp, "invalid_destination_ip"],
    ["isMissingPair", isMissingPair, "missing_pair"],
    ["isUnexpectedPairPayload", isUnexpectedPairPayload, "unexpected_pair_payload"],
    ["isInvalidEvaluationPayload", isInvalidEvaluationPayload, "invalid_evaluation_payload"],
  ])("%s matches its own code and rejects every other", (_name, matcher, expectedCode) => {
    expect(matcher(apiError(expectedCode))).toBe(true);
    expect(matcher(apiError("something_else"))).toBe(false);
    expect(matcher(new Error("no cause"))).toBe(false);
    expect(matcher(null)).toBe(false);
  });
});

describe("stateBadgeVariant", () => {
  test("maps every CampaignState to a Badge variant", () => {
    expect(stateBadgeVariant("draft")).toBe("outline");
    expect(stateBadgeVariant("running")).toBe("default");
    expect(stateBadgeVariant("completed")).toBe("secondary");
    expect(stateBadgeVariant("evaluated")).toBe("secondary");
    expect(stateBadgeVariant("stopped")).toBe("destructive");
  });
});
