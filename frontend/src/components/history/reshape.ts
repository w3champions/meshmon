/**
 * Time-series reshape helpers for the `/history/pair` chart.
 *
 * `HistoryMeasurement` rows arrive one row per `(protocol, measured_at)`
 * tuple. recharts wants one row per time bucket with each protocol's series
 * as flat keys so `<Line>` / `<Area>` / `<Bar>` can overlay them on the
 * same X axis.
 *
 * recharts 3.8 does NOT accept a `[low, high]` tuple `dataKey` on `<Area>`
 * for a shaded band. The workaround is to emit both `*_min` and a
 * `*_range_delta` (= max - min) key and render the band as two stacked
 * `<Area>` components sharing a `stackId`. The first area plots the
 * transparent baseline at `*_min`; the second plots the delta on top with
 * the band's fill colour. Together they render a filled min/max envelope.
 */

import type { ProbeProtocol } from "@/api/hooks/campaigns";
import type { HistoryMeasurement } from "@/api/hooks/history";

/** Every protocol the chart can plot. */
export const CHART_PROTOCOLS: readonly ProbeProtocol[] = ["icmp", "tcp", "udp"] as const;

/**
 * One row per `measured_at` bucket with flat per-protocol keys.
 *
 * Absent keys (e.g. a bucket that only carries ICMP) are omitted rather
 * than set to `null` — recharts treats missing keys as gaps in the line,
 * which is what we want. `_range_delta` is only emitted when both `*_min`
 * and `*_max` are present on at least one row for the bucket.
 */
export interface ChartRow {
  /** RFC 3339 timestamp — `measured_at` verbatim, used as the XAxis dataKey. */
  t: string;

  icmp_avg?: number;
  icmp_min?: number;
  icmp_max?: number;
  icmp_range_delta?: number;
  icmp_loss?: number;

  tcp_avg?: number;
  tcp_min?: number;
  tcp_max?: number;
  tcp_range_delta?: number;
  tcp_loss?: number;

  udp_avg?: number;
  udp_min?: number;
  udp_max?: number;
  udp_range_delta?: number;
  udp_loss?: number;
}

/**
 * Reshape a flat `HistoryMeasurement[]` into chart rows keyed by
 * `measured_at`. When two rows share a bucket + protocol (should not
 * happen at the backend level, but tolerate it) the later row wins.
 */
export function reshapeForChart(measurements: readonly HistoryMeasurement[]): ChartRow[] {
  const buckets = new Map<string, ChartRow>();
  for (const m of measurements) {
    const existing = buckets.get(m.measured_at);
    const row: ChartRow = existing ?? { t: m.measured_at };
    // Per-key writes go through a `Record<string, number>` bridge view —
    // `ChartRow` is a closed interface, but the keys we set all come from
    // `CHART_PROTOCOLS` so the runtime shape stays safe.
    const bag = row as unknown as Record<string, number>;
    const proto = m.protocol;
    if (m.latency_avg_ms != null) bag[`${proto}_avg`] = m.latency_avg_ms;
    if (m.latency_min_ms != null) bag[`${proto}_min`] = m.latency_min_ms;
    if (m.latency_max_ms != null) bag[`${proto}_max`] = m.latency_max_ms;
    if (m.latency_min_ms != null && m.latency_max_ms != null) {
      bag[`${proto}_range_delta`] = m.latency_max_ms - m.latency_min_ms;
    }
    bag[`${proto}_loss`] = m.loss_pct;
    buckets.set(m.measured_at, row);
  }
  return [...buckets.values()].sort((a, b) => a.t.localeCompare(b.t));
}

/**
 * Which protocols appear in the reshaped dataset — drives the `<Line>` /
 * `<Area>` / `<Bar>` fan-out so PairChart doesn't render empty series.
 */
export function protocolsPresent(rows: readonly ChartRow[]): ProbeProtocol[] {
  const present: Set<ProbeProtocol> = new Set();
  for (const row of rows) {
    const bag = row as unknown as Record<string, unknown>;
    for (const p of CHART_PROTOCOLS) {
      if (bag[`${p}_avg`] !== undefined) present.add(p);
      else if (bag[`${p}_loss`] !== undefined) present.add(p);
    }
  }
  return CHART_PROTOCOLS.filter((p) => present.has(p));
}
