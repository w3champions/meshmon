import { describe, expect, test } from "vitest";
import {
  classify,
  DEFAULT_LIVENESS_THRESHOLDS,
  getAgentLiveness,
  isStale,
  type LivenessThresholds,
  thresholdsFromConfig,
} from "@/lib/health";

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

  test("returns false at exactly 5 min boundary (exclusive)", () => {
    const fiveMinutesAgo = "2026-04-16T11:55:00Z";
    // 5 * 60 * 1000 ms apart — the check is `> threshold` (strict), so returns false.
    expect(isStale(fiveMinutesAgo, now)).toBe(false);
  });
});

describe("getAgentLiveness", () => {
  // 5 min offline, 30 s stale — wider stale band than the defaults so
  // the test fixtures sit unambiguously in each region.
  const cfg: LivenessThresholds = {
    offlineAfterMs: 5 * 60_000,
    staleAfterMs: 30_000,
  };
  const now = Date.parse("2026-04-16T12:00:00Z");

  test("returns 'online' for a fresh push within the stale window", () => {
    expect(getAgentLiveness("2026-04-16T11:59:50Z", cfg, now)).toBe("online");
  });

  test("returns 'stale' between the stale and offline thresholds", () => {
    // 1 min ago → past 30 s stale, well inside 5 min offline.
    expect(getAgentLiveness("2026-04-16T11:59:00Z", cfg, now)).toBe("stale");
  });

  test("returns 'offline' past the offline threshold", () => {
    // 6 min ago.
    expect(getAgentLiveness("2026-04-16T11:54:00Z", cfg, now)).toBe("offline");
  });

  test("treats future timestamps (clock skew) as online", () => {
    expect(getAgentLiveness("2026-04-16T12:00:30Z", cfg, now)).toBe("online");
  });

  test("returns 'offline' for unparseable strings", () => {
    expect(getAgentLiveness("not-a-date", cfg, now)).toBe("offline");
  });

  test("falls back to library defaults when no thresholds passed", () => {
    // Default offline = 5 min, stale = 20 s.
    // 10 s ago → online.
    expect(getAgentLiveness(new Date(now - 10_000).toISOString(), undefined, now)).toBe("online");
    // 60 s ago → stale.
    expect(getAgentLiveness(new Date(now - 60_000).toISOString(), undefined, now)).toBe("stale");
  });

  test("default thresholds match the constants exposed for the loader fallback", () => {
    expect(DEFAULT_LIVENESS_THRESHOLDS.offlineAfterMs).toBe(5 * 60_000);
    expect(DEFAULT_LIVENESS_THRESHOLDS.staleAfterMs).toBe(20_000);
  });
});

describe("thresholdsFromConfig", () => {
  test("projects minutes/seconds onto millisecond thresholds", () => {
    const out = thresholdsFromConfig({
      target_active_window_minutes: 5,
      refresh_interval_seconds: 10,
    });
    expect(out.offlineAfterMs).toBe(300_000);
    // Stale = 2× refresh interval — covers one missed registry refresh.
    expect(out.staleAfterMs).toBe(20_000);
  });

  test("respects operator overrides without baking in defaults", () => {
    const out = thresholdsFromConfig({
      target_active_window_minutes: 1,
      refresh_interval_seconds: 30,
    });
    expect(out.offlineAfterMs).toBe(60_000);
    expect(out.staleAfterMs).toBe(60_000);
  });
});
