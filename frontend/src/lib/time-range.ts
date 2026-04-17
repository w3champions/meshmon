export type TimeRangeKey = "1h" | "6h" | "24h" | "7d" | "30d" | "2y" | "custom";

export const TIME_RANGES: TimeRangeKey[] = ["1h", "6h", "24h", "7d", "30d", "2y", "custom"];

const PRESET_KEYS: Exclude<TimeRangeKey, "custom">[] = ["1h", "6h", "24h", "7d", "30d", "2y"];

const PRESET_MILLIS: Record<Exclude<TimeRangeKey, "custom">, number> = {
  "1h": 60 * 60 * 1000,
  "6h": 6 * 60 * 60 * 1000,
  "24h": 24 * 60 * 60 * 1000,
  "7d": 7 * 24 * 60 * 60 * 1000,
  "30d": 30 * 24 * 60 * 60 * 1000,
  "2y": 2 * 365 * 24 * 60 * 60 * 1000,
};

export interface CustomRange {
  from: Date;
  to: Date;
}

export interface GrafanaRange {
  from: string;
  to: string;
}

/**
 * Convert a preset key (or custom bounds) into the `{from, to}` pair that
 * Grafana's `d-solo` endpoint accepts.
 *
 * Presets map to the `now-<key>` / `now` shorthand that Grafana resolves
 * server-side. Custom ranges return millisecond-epoch strings.
 */
export function grafanaTimes(key: TimeRangeKey, custom?: CustomRange): GrafanaRange {
  if (key === "custom") {
    if (!custom) return { from: "now-1h", to: "now" };
    return { from: String(custom.from.getTime()), to: String(custom.to.getTime()) };
  }
  return { from: `now-${key}`, to: "now" };
}

/**
 * Resolve a preset key (or custom bounds) into absolute `Date` bounds. Used
 * for Prometheus-style API queries that require concrete timestamps.
 */
export function rangeBounds(
  key: TimeRangeKey,
  custom?: CustomRange,
  now: Date = new Date(),
): { from: Date; to: Date } {
  if (key === "custom") {
    if (!custom) {
      return { from: new Date(now.getTime() - PRESET_MILLIS["1h"]), to: now };
    }
    return { from: custom.from, to: custom.to };
  }
  return { from: new Date(now.getTime() - PRESET_MILLIS[key]), to: now };
}

export interface TimeRangeSearch {
  range: TimeRangeKey;
  custom?: CustomRange;
}

function isPresetKey(value: unknown): value is Exclude<TimeRangeKey, "custom"> {
  return typeof value === "string" && (PRESET_KEYS as string[]).includes(value);
}

/**
 * Parse a `{ range, from, to }` search-param bag into a `TimeRangeSearch`.
 *
 * Invalid / missing values fall back to the `24h` default so callers never
 * have to branch on malformed URLs.
 */
export function parseTimeRangeSearch(input: {
  range?: unknown;
  from?: unknown;
  to?: unknown;
}): TimeRangeSearch {
  const raw = input.range;

  if (raw === "custom") {
    const fromStr = typeof input.from === "string" ? input.from : undefined;
    const toStr = typeof input.to === "string" ? input.to : undefined;
    if (!fromStr || !toStr) return { range: "24h" };
    const from = new Date(fromStr);
    const to = new Date(toStr);
    if (Number.isNaN(from.getTime()) || Number.isNaN(to.getTime())) {
      return { range: "24h" };
    }
    return { range: "custom", custom: { from, to } };
  }

  if (isPresetKey(raw)) return { range: raw };
  return { range: "24h" };
}
