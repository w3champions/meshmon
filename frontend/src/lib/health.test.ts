import { describe, expect, test } from "vitest";
import { classify, isStale } from "@/lib/health";

describe("classify", () => {
  test("rate below 5% is normal", () => {
    expect(classify(0)).toBe("normal");
    expect(classify(0.04999)).toBe("normal");
  });

  test("rate in [5%, 20%) is degraded", () => {
    expect(classify(0.05)).toBe("degraded");
    expect(classify(0.1)).toBe("degraded");
    expect(classify(0.19)).toBe("degraded");
  });

  test("rate >= 20% is unreachable", () => {
    expect(classify(0.2)).toBe("unreachable");
    expect(classify(0.99)).toBe("unreachable");
    expect(classify(1)).toBe("unreachable");
  });

  test("missing or NaN rate is stale", () => {
    expect(classify(undefined)).toBe("stale");
    expect(classify(Number.NaN)).toBe("stale");
  });
});

describe("isStale", () => {
  const now = Date.parse("2026-04-16T12:00:00Z");

  test("returns false when last_seen is within 5 min", () => {
    expect(isStale("2026-04-16T11:58:00Z", now)).toBe(false);
  });

  test("returns true when last_seen is older than 5 min", () => {
    expect(isStale("2026-04-16T11:54:00Z", now)).toBe(true);
  });

  test("returns true for unparseable strings", () => {
    expect(isStale("garbage", now)).toBe(true);
  });
});
