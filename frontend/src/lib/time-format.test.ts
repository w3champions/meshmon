import { describe, expect, it } from "vitest";
import {
  dayBoundariesBetween,
  formatClockUtc,
  formatClockUtcSec,
  formatDelta,
  formatRelativeAgo,
  formatShortDate,
  formatTickLabel,
  isSameDayUtc,
} from "./time-format";

const MS_SEC = 1_000;
const MS_MIN = 60 * MS_SEC;
const MS_HOUR = 60 * MS_MIN;
const MS_DAY = 24 * MS_HOUR;

describe("formatDelta", () => {
  it("formats sub-minute deltas in seconds", () => {
    expect(formatDelta(46 * MS_SEC)).toBe("46s");
  });
  it("formats sub-hour deltas as Xm Ys (dropping trailing 0s)", () => {
    expect(formatDelta(2 * MS_MIN + 54 * MS_SEC)).toBe("2m 54s");
    expect(formatDelta(5 * MS_MIN)).toBe("5m");
  });
  it("formats sub-day deltas as Xh Ym", () => {
    expect(formatDelta(3 * MS_HOUR + 12 * MS_MIN)).toBe("3h 12m");
  });
  it("formats multi-day deltas as Xd Yh", () => {
    expect(formatDelta(2 * MS_DAY + 4 * MS_HOUR)).toBe("2d 4h");
    expect(formatDelta(97 * MS_DAY)).toBe("97d");
  });
  it("handles negative deltas by magnitude (caller disambiguates direction)", () => {
    expect(formatDelta(-46 * MS_SEC)).toBe("46s");
  });
  it("rolls a rounded sub-unit into the parent (no '60s', no '3m 60s')", () => {
    expect(formatDelta(59_500)).toBe("1m"); // not "60s"
    expect(formatDelta(3 * 60_000 + 59_500)).toBe("3m 59s"); // not "3m 60s"
    expect(formatDelta(3 * 3_600_000 + 59 * 60_000 + 30_000)).toBe("3h 59m"); // not "3h 60m"
    expect(formatDelta(2 * 86_400_000 + 23 * 3_600_000 + 30 * 60_000)).toBe("2d 23h"); // not "2d 24h"
  });
});

describe("formatRelativeAgo", () => {
  const now = Date.UTC(2026, 3, 17, 9, 17, 0); // Apr 17 2026 09:17:00 UTC
  it("returns 'just now' under 45s", () => {
    expect(formatRelativeAgo(now - 10 * MS_SEC, now)).toBe("just now");
  });
  it("returns N min ago under an hour", () => {
    expect(formatRelativeAgo(now - 5 * MS_MIN, now)).toBe("5 min ago");
  });
  it("returns N h ago under a day", () => {
    expect(formatRelativeAgo(now - 2 * MS_HOUR, now)).toBe("2h ago");
  });
  it("returns 'yesterday' when target is on the prior UTC day", () => {
    const yesterday = Date.UTC(2026, 3, 16, 23, 55, 0);
    expect(formatRelativeAgo(yesterday, now)).toBe("yesterday");
  });
  it("returns N days ago within a week", () => {
    expect(formatRelativeAgo(now - 3 * MS_DAY, now)).toBe("3 days ago");
  });
  it("returns absolute short date beyond a week", () => {
    const apr5 = Date.UTC(2026, 3, 5, 12, 0, 0);
    expect(formatRelativeAgo(apr5, now)).toBe("Apr 5");
  });
  it("does not read 23h ago as 'Nh ago' when across a UTC midnight", () => {
    const now = Date.UTC(2026, 3, 17, 0, 10, 0);
    const target = Date.UTC(2026, 3, 16, 23, 50, 0);
    expect(formatRelativeAgo(target, now)).toBe("yesterday");
  });
  it("shows 'just now' for small future skew (< 45s)", () => {
    expect(formatRelativeAgo(now + 10 * MS_SEC, now)).toBe("just now");
  });
  it("falls back to absolute date for large future drift, never '-Nh ago'", () => {
    expect(formatRelativeAgo(now + 5 * MS_MIN, now)).toBe("Apr 17");
    expect(formatRelativeAgo(now + 2 * MS_HOUR, now)).toBe("Apr 17");
  });
});

describe("formatClockUtc / formatClockUtcSec", () => {
  const ms = Date.UTC(2026, 3, 17, 9, 12, 4);
  it("formats HH:MM in UTC", () => {
    expect(formatClockUtc(ms)).toBe("09:12");
  });
  it("formats HH:MM:SS in UTC", () => {
    expect(formatClockUtcSec(ms)).toBe("09:12:04");
  });
});

describe("formatShortDate", () => {
  const apr17 = Date.UTC(2026, 3, 17, 0, 0, 0);
  it("omits the year by default", () => {
    expect(formatShortDate(apr17)).toBe("Apr 17");
  });
  it("includes the year when asked", () => {
    expect(formatShortDate(apr17, true)).toBe("Apr 17, 2026");
  });
});

describe("isSameDayUtc", () => {
  it("true across times on the same UTC day", () => {
    expect(isSameDayUtc(Date.UTC(2026, 3, 17, 0, 0, 1), Date.UTC(2026, 3, 17, 23, 59, 59))).toBe(
      true,
    );
  });
  it("false across a UTC midnight", () => {
    expect(isSameDayUtc(Date.UTC(2026, 3, 16, 23, 59, 59), Date.UTC(2026, 3, 17, 0, 0, 0))).toBe(
      false,
    );
  });
});

describe("dayBoundariesBetween", () => {
  it("returns [] when range stays inside one UTC day", () => {
    const a = Date.UTC(2026, 3, 17, 0, 0, 1);
    const b = Date.UTC(2026, 3, 17, 23, 0, 0);
    expect(dayBoundariesBetween(a, b)).toEqual([]);
  });
  it("returns the next UTC midnight when the range crosses one day", () => {
    const a = Date.UTC(2026, 3, 16, 23, 55, 0);
    const b = Date.UTC(2026, 3, 17, 0, 5, 0);
    expect(dayBoundariesBetween(a, b)).toEqual([Date.UTC(2026, 3, 17, 0, 0, 0)]);
  });
  it("returns every crossed UTC midnight", () => {
    const a = Date.UTC(2026, 3, 16, 23, 55, 0);
    const b = Date.UTC(2026, 3, 19, 0, 5, 0);
    expect(dayBoundariesBetween(a, b)).toEqual([
      Date.UTC(2026, 3, 17, 0, 0, 0),
      Date.UTC(2026, 3, 18, 0, 0, 0),
      Date.UTC(2026, 3, 19, 0, 0, 0),
    ]);
  });
  it("does not include toMs itself when it falls exactly on UTC midnight", () => {
    const a = Date.UTC(2026, 3, 16, 23, 55, 0);
    const b = Date.UTC(2026, 3, 17, 0, 0, 0);
    expect(dayBoundariesBetween(a, b)).toEqual([]);
  });
});

describe("formatTickLabel", () => {
  const t0 = Date.UTC(2026, 3, 17, 9, 0, 0);
  const t1 = Date.UTC(2026, 3, 17, 9, 5, 0);
  const t2 = Date.UTC(2026, 3, 17, 9, 14, 41);

  it("uses HH:MM when all ticks are same UTC day", () => {
    const ctx = { allTicksMs: [t0, t1, t2], selectedMs: t2 };
    expect(formatTickLabel(t1, ctx, false)).toBe("09:05");
    expect(formatTickLabel(t2, ctx, true)).toBe("09:14");
  });

  it("uses MMM DD when ticks span >18 h, and adds HH:MM on the selected tick", () => {
    const day1 = Date.UTC(2026, 0, 10, 14, 32, 0); // Jan 10 14:32
    const day2 = Date.UTC(2026, 3, 17, 18, 41, 0); // Apr 17 18:41
    const ctx = { allTicksMs: [day1, day2], selectedMs: day2 };
    expect(formatTickLabel(day1, ctx, false)).toBe("Jan 10");
    expect(formatTickLabel(day2, ctx, true)).toBe("Apr 17 · 18:41");
  });
});
