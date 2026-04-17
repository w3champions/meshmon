const MS_SEC = 1_000;
const MS_MIN = 60 * MS_SEC;
const MS_HOUR = 60 * MS_MIN;
const MS_DAY = 24 * MS_HOUR;
const MS_WEEK = 7 * MS_DAY;
const RAIL_DATE_THRESHOLD_MS = 18 * MS_HOUR;

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

export interface TickFormatCtx {
  allTicksMs: number[];
  selectedMs: number;
}

export function formatClockUtc(ms: number): string {
  const d = new Date(ms);
  return `${pad2(d.getUTCHours())}:${pad2(d.getUTCMinutes())}`;
}

export function formatClockUtcSec(ms: number): string {
  const d = new Date(ms);
  return `${pad2(d.getUTCHours())}:${pad2(d.getUTCMinutes())}:${pad2(d.getUTCSeconds())}`;
}

export function formatShortDate(ms: number, includeYear = false): string {
  const d = new Date(ms);
  const base = `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}`;
  return includeYear ? `${base}, ${d.getUTCFullYear()}` : base;
}

export function formatDelta(deltaMs: number): string {
  const abs = Math.abs(deltaMs);
  if (abs < MS_MIN) {
    const secs = Math.round(abs / MS_SEC);
    // If rounding pushed us to 60s, cascade to "1m" rather than emit "60s".
    return secs < 60 ? `${secs}s` : "1m";
  }
  if (abs < MS_HOUR) {
    const mins = Math.floor(abs / MS_MIN);
    const secs = Math.floor((abs % MS_MIN) / MS_SEC);
    return secs > 0 ? `${mins}m ${secs}s` : `${mins}m`;
  }
  if (abs < MS_DAY) {
    const hours = Math.floor(abs / MS_HOUR);
    const mins = Math.floor((abs % MS_HOUR) / MS_MIN);
    return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  }
  const days = Math.floor(abs / MS_DAY);
  const hours = Math.floor((abs % MS_DAY) / MS_HOUR);
  return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
}

export function formatRelativeAgo(targetMs: number, nowMs: number): string {
  const delta = nowMs - targetMs;
  if (delta < 45 * MS_SEC) return "just now";
  // Only fall into "N min ago" / "Nh ago" when the target is still the same
  // UTC day as now. Otherwise we step up the ladder (yesterday → N days ago →
  // absolute date) so that a target from the previous UTC day never reads as
  // "20 min ago" or "9h ago" just because it's numerically close to now.
  if (isSameDayUtc(targetMs, nowMs)) {
    if (delta < MS_HOUR) return `${Math.round(delta / MS_MIN)} min ago`;
    return `${Math.round(delta / MS_HOUR)}h ago`;
  }
  if (isYesterdayUtc(targetMs, nowMs)) return "yesterday";
  if (delta < MS_WEEK) return `${Math.floor(delta / MS_DAY)} days ago`;
  return formatShortDate(targetMs);
}

export function isSameDayUtc(aMs: number, bMs: number): boolean {
  const a = new Date(aMs);
  const b = new Date(bMs);
  return (
    a.getUTCFullYear() === b.getUTCFullYear() &&
    a.getUTCMonth() === b.getUTCMonth() &&
    a.getUTCDate() === b.getUTCDate()
  );
}

export function dayBoundariesBetween(fromMs: number, toMs: number): number[] {
  if (toMs <= fromMs) return [];
  const boundaries: number[] = [];
  const first = nextUtcMidnight(fromMs);
  for (let t = first; t < toMs; t += MS_DAY) boundaries.push(t);
  return boundaries;
}

export function formatTickLabel(tickMs: number, ctx: TickFormatCtx, isSelected: boolean): string {
  const span = rangeMs(ctx.allTicksMs);
  if (span < RAIL_DATE_THRESHOLD_MS) return formatClockUtc(tickMs);
  const base = formatShortDate(tickMs);
  return isSelected ? `${base} · ${formatClockUtc(tickMs)}` : base;
}

function pad2(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

function rangeMs(values: number[]): number {
  if (values.length < 2) return 0;
  let lo = values[0];
  let hi = values[0];
  for (const v of values) {
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  return hi - lo;
}

function nextUtcMidnight(ms: number): number {
  const d = new Date(ms);
  return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate() + 1, 0, 0, 0, 0);
}

function isYesterdayUtc(targetMs: number, nowMs: number): boolean {
  const n = new Date(nowMs);
  const yesterday = Date.UTC(n.getUTCFullYear(), n.getUTCMonth(), n.getUTCDate() - 1);
  const t = new Date(targetMs);
  return Date.UTC(t.getUTCFullYear(), t.getUTCMonth(), t.getUTCDate()) === yesterday;
}
