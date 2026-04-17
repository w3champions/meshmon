import { describe, expect, test } from "vitest";
import {
  grafanaTimes,
  parseTimeRangeSearch,
  rangeBounds,
  TIME_RANGES,
  type TimeRangeKey,
} from "@/lib/time-range";

describe("TIME_RANGES", () => {
  test("includes all expected presets plus custom", () => {
    expect(TIME_RANGES).toEqual(["1h", "6h", "24h", "7d", "30d", "2y", "custom"]);
  });
});

describe("grafanaTimes", () => {
  test("maps presets to grafana `now-*` / `now`", () => {
    expect(grafanaTimes("1h")).toEqual({ from: "now-1h", to: "now" });
    expect(grafanaTimes("6h")).toEqual({ from: "now-6h", to: "now" });
    expect(grafanaTimes("24h")).toEqual({ from: "now-24h", to: "now" });
    expect(grafanaTimes("7d")).toEqual({ from: "now-7d", to: "now" });
    expect(grafanaTimes("30d")).toEqual({ from: "now-30d", to: "now" });
    expect(grafanaTimes("2y")).toEqual({ from: "now-2y", to: "now" });
  });

  test("custom returns epoch-millisecond strings from the supplied bounds", () => {
    const from = new Date("2026-04-10T00:00:00Z");
    const to = new Date("2026-04-11T00:00:00Z");
    expect(grafanaTimes("custom", { from, to })).toEqual({
      from: String(from.getTime()),
      to: String(to.getTime()),
    });
  });

  test("custom without bounds falls back to 1h preset", () => {
    expect(grafanaTimes("custom")).toEqual({ from: "now-1h", to: "now" });
  });
});

describe("rangeBounds", () => {
  const now = new Date("2026-04-17T12:00:00Z");

  test("1h preset returns [now-1h, now]", () => {
    const { from, to } = rangeBounds("1h", undefined, now);
    expect(to).toEqual(now);
    expect(from).toEqual(new Date(now.getTime() - 60 * 60 * 1000));
  });

  test("24h preset returns [now-24h, now]", () => {
    const { from, to } = rangeBounds("24h", undefined, now);
    expect(to).toEqual(now);
    expect(from).toEqual(new Date(now.getTime() - 24 * 60 * 60 * 1000));
  });

  test("7d preset returns [now-7d, now]", () => {
    const { from, to } = rangeBounds("7d", undefined, now);
    expect(to).toEqual(now);
    expect(from).toEqual(new Date(now.getTime() - 7 * 24 * 60 * 60 * 1000));
  });

  test("2y preset returns approximately [now - 2*365d, now]", () => {
    const { from, to } = rangeBounds("2y", undefined, now);
    expect(to).toEqual(now);
    expect(from).toEqual(new Date(now.getTime() - 2 * 365 * 24 * 60 * 60 * 1000));
  });

  test("custom passes through supplied bounds", () => {
    const customFrom = new Date("2026-04-01T00:00:00Z");
    const customTo = new Date("2026-04-02T00:00:00Z");
    const { from, to } = rangeBounds("custom", { from: customFrom, to: customTo }, now);
    expect(from).toEqual(customFrom);
    expect(to).toEqual(customTo);
  });

  test("custom without bounds falls back to 1h window", () => {
    const { from, to } = rangeBounds("custom", undefined, now);
    expect(to).toEqual(now);
    expect(from).toEqual(new Date(now.getTime() - 60 * 60 * 1000));
  });
});

describe("parseTimeRangeSearch", () => {
  test("returns '24h' default when range is missing or invalid", () => {
    expect(parseTimeRangeSearch({}).range).toBe("24h");
    expect(parseTimeRangeSearch({ range: "invalid" }).range).toBe("24h");
  });

  test("returns recognised preset keys", () => {
    for (const key of ["1h", "6h", "24h", "7d", "30d", "2y"] satisfies TimeRangeKey[]) {
      expect(parseTimeRangeSearch({ range: key }).range).toBe(key);
    }
  });

  test("returns 'custom' with parsed from/to Date pair when both provided", () => {
    const fromIso = "2026-04-10T00:00:00Z";
    const toIso = "2026-04-11T00:00:00Z";
    const result = parseTimeRangeSearch({ range: "custom", from: fromIso, to: toIso });
    expect(result.range).toBe("custom");
    expect(result.custom).toEqual({
      from: new Date(fromIso),
      to: new Date(toIso),
    });
  });

  test("custom without from/to falls back to '24h'", () => {
    expect(parseTimeRangeSearch({ range: "custom" }).range).toBe("24h");
  });

  test("custom with unparseable bounds falls back to '24h'", () => {
    const result = parseTimeRangeSearch({
      range: "custom",
      from: "garbage",
      to: "more-garbage",
    });
    expect(result.range).toBe("24h");
    expect(result.custom).toBeUndefined();
  });
});
