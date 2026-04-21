import { describe, expect, test } from "vitest";
import { parseHistoryPairSearch } from "@/router/index";

// ---------------------------------------------------------------------------
// parseHistoryPairSearch — route-level resilience.
//
// A shared URL can arrive with values the schema would normally reject
// (garbage datetimes, an unknown protocol token, a `range=custom` that's
// been clipped of its bounds). Route validation must degrade to a sane
// default instead of throwing, otherwise the operator is locked out of
// the page with an unhelpful "validation failed" error.
// ---------------------------------------------------------------------------

describe("parseHistoryPairSearch", () => {
  test("passes valid search through unchanged", () => {
    const parsed = parseHistoryPairSearch({
      source: "agent-a",
      destination: "10.0.0.1",
      protocol: ["icmp", "tcp"],
      range: "7d",
    });
    expect(parsed).toEqual({
      source: "agent-a",
      destination: "10.0.0.1",
      protocol: ["icmp", "tcp"],
      range: "7d",
      from: undefined,
      to: undefined,
    });
  });

  test("passes a valid custom range through", () => {
    const parsed = parseHistoryPairSearch({
      source: "agent-a",
      destination: "10.0.0.1",
      range: "custom",
      from: "2026-04-13T10:00:00.000Z",
      to: "2026-04-13T14:00:00.000Z",
    });
    expect(parsed.range).toBe("custom");
    expect(parsed.from).toBe("2026-04-13T10:00:00.000Z");
    expect(parsed.to).toBe("2026-04-13T14:00:00.000Z");
  });

  test("drops the range back to default when `custom` is missing its bounds", () => {
    // A shared URL clipped of `from`/`to` would otherwise throw the
    // schema's `.refine(...)`. Route-level parse must fall back rather
    // than hard-failing navigation.
    const parsed = parseHistoryPairSearch({
      source: "agent-a",
      destination: "10.0.0.1",
      range: "custom",
    });
    expect(parsed.range).toBe("30d");
    expect(parsed.from).toBeUndefined();
    expect(parsed.to).toBeUndefined();
    // Sibling params must survive the retry.
    expect(parsed.source).toBe("agent-a");
    expect(parsed.destination).toBe("10.0.0.1");
  });

  test("drops garbage datetime fields and keeps the rest", () => {
    const parsed = parseHistoryPairSearch({
      source: "agent-a",
      range: "24h",
      from: "not-a-datetime",
      to: "also-garbage",
    });
    expect(parsed.range).toBe("24h");
    expect(parsed.from).toBeUndefined();
    expect(parsed.to).toBeUndefined();
    expect(parsed.source).toBe("agent-a");
  });

  test("drops an unknown protocol token and keeps siblings", () => {
    const parsed = parseHistoryPairSearch({
      source: "agent-a",
      protocol: ["icmp", "not-a-protocol"],
      range: "7d",
    });
    expect(parsed.protocol).toBeUndefined();
    expect(parsed.source).toBe("agent-a");
    expect(parsed.range).toBe("7d");
  });

  test("falls back to default range on a non-object input", () => {
    expect(parseHistoryPairSearch(null).range).toBe("30d");
    expect(parseHistoryPairSearch("something").range).toBe("30d");
    expect(parseHistoryPairSearch(undefined).range).toBe("30d");
  });
});
