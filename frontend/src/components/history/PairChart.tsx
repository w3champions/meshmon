import { format } from "date-fns";
import { useMemo } from "react";
import {
  Area,
  Bar,
  CartesianGrid,
  ComposedChart,
  Legend,
  Line,
  ResponsiveContainer,
  Tooltip,
  type TooltipContentProps,
  XAxis,
  YAxis,
} from "recharts";
import type { ProbeProtocol } from "@/api/hooks/campaigns";
import type { HistoryMeasurement } from "@/api/hooks/history";
import {
  CHART_PROTOCOLS,
  type ChartRow,
  protocolsPresent,
  reshapeForChart,
} from "@/components/history/reshape";
import { cn } from "@/lib/utils";

/**
 * Per-protocol colour tokens. Kept decorative — meaning lives in the
 * legend + tooltip labels so operators with colour-vision deficiency can
 * still disambiguate.
 */
const PROTOCOL_COLORS: Record<ProbeProtocol, string> = {
  icmp: "var(--color-chart-1, #38bdf8)", // sky
  tcp: "var(--color-chart-2, #a78bfa)", // violet
  udp: "var(--color-chart-3, #f97316)", // orange
};

interface PairChartProps {
  measurements: readonly HistoryMeasurement[];
  className?: string;
}

/**
 * Latency + loss time-series for a single (source, destination) pair.
 *
 * Two stacked `ComposedChart`s share the reshape helper — a combined
 * chart reads as muddy when all three protocols are active, so latency
 * stacks above loss with a common X axis. Each protocol renders:
 *   - min/max shaded band (two stacked `<Area>`s sharing `stackId`)
 *   - avg line on top (`<Line>`)
 *   - loss bars on the lower chart (`<Bar>`)
 */
export function PairChart({ measurements, className }: PairChartProps) {
  const rows = useMemo(() => reshapeForChart(measurements), [measurements]);
  const protocols = useMemo(() => protocolsPresent(rows), [rows]);

  if (rows.length === 0) {
    return (
      <div
        role="status"
        className={cn(
          "flex h-64 w-full items-center justify-center rounded border text-sm text-muted-foreground",
          className,
        )}
      >
        No measurements in the selected window.
      </div>
    );
  }

  return (
    <fieldset className={cn("flex flex-col gap-4 border-0 p-0", className)}>
      <legend className="sr-only">Pair chart</legend>
      <div
        role="img"
        aria-label="Latency over time"
        className="h-64 w-full"
        data-testid="pair-chart-latency"
      >
        <ResponsiveContainer width="100%" height="100%">
          <ComposedChart data={rows} margin={{ top: 8, right: 16, left: 8, bottom: 0 }}>
            <CartesianGrid strokeDasharray="3 3" opacity={0.3} />
            <XAxis
              dataKey="t"
              tickFormatter={formatTick}
              minTickGap={40}
              stroke="currentColor"
              fontSize={12}
            />
            <YAxis
              unit=" ms"
              stroke="currentColor"
              fontSize={12}
              width={64}
              allowDecimals={false}
            />
            <Tooltip content={LatencyTooltip} />
            <Legend />
            {protocols.map((p) => (
              <Area
                key={`${p}-baseline`}
                type="monotone"
                dataKey={`${p}_min`}
                stackId={`${p}_band`}
                stroke="none"
                fill="none"
                legendType="none"
                isAnimationActive={false}
                name={`${p.toUpperCase()} min/max band`}
              />
            ))}
            {protocols.map((p) => (
              <Area
                key={`${p}-band`}
                type="monotone"
                dataKey={`${p}_range_delta`}
                stackId={`${p}_band`}
                stroke="none"
                fill={PROTOCOL_COLORS[p]}
                fillOpacity={0.15}
                isAnimationActive={false}
                name={`${p.toUpperCase()} min/max`}
              />
            ))}
            {protocols.map((p) => (
              <Line
                key={`${p}-avg`}
                type="monotone"
                dataKey={`${p}_avg`}
                stroke={PROTOCOL_COLORS[p]}
                strokeWidth={2}
                dot={false}
                isAnimationActive={false}
                name={`${p.toUpperCase()} avg latency`}
                connectNulls
              />
            ))}
          </ComposedChart>
        </ResponsiveContainer>
      </div>
      <div
        role="img"
        aria-label="Packet loss over time"
        className="h-40 w-full"
        data-testid="pair-chart-loss"
      >
        <ResponsiveContainer width="100%" height="100%">
          <ComposedChart data={rows} margin={{ top: 0, right: 16, left: 8, bottom: 16 }}>
            <CartesianGrid strokeDasharray="3 3" opacity={0.3} />
            <XAxis
              dataKey="t"
              tickFormatter={formatTick}
              minTickGap={40}
              stroke="currentColor"
              fontSize={12}
            />
            <YAxis
              unit="%"
              domain={[0, 100]}
              stroke="currentColor"
              fontSize={12}
              width={48}
              allowDecimals={false}
            />
            <Tooltip content={LossTooltip} />
            <Legend />
            {protocols.map((p) => (
              <Bar
                key={`${p}-loss`}
                dataKey={`${p}_loss`}
                fill={PROTOCOL_COLORS[p]}
                fillOpacity={0.6}
                isAnimationActive={false}
                name={`${p.toUpperCase()} loss`}
              />
            ))}
          </ComposedChart>
        </ResponsiveContainer>
      </div>
    </fieldset>
  );
}

function formatTick(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return format(d, "MMM d HH:mm");
}

function formatFullTimestamp(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return format(d, "yyyy-MM-dd HH:mm:ss");
}

function LatencyTooltip(props: TooltipContentProps) {
  const { active, payload, label } = props;
  if (!active || !payload || payload.length === 0 || typeof label !== "string") return null;
  const row = payload[0]?.payload as ChartRow | undefined;
  if (!row) return null;
  return (
    <div
      role="tooltip"
      className="rounded border bg-background/95 p-2 text-xs shadow-md backdrop-blur-sm"
    >
      <div className="mb-1 font-medium">{formatFullTimestamp(label)}</div>
      <ul className="space-y-0.5">
        {CHART_PROTOCOLS.filter((p) => row[`${p}_avg`] !== undefined).map((p) => {
          const avg = row[`${p}_avg`];
          const min = row[`${p}_min`];
          const max = row[`${p}_max`];
          return (
            <li key={p} className="flex items-center gap-2">
              <span
                aria-hidden
                className="inline-block h-2 w-2 rounded-sm"
                style={{ background: PROTOCOL_COLORS[p] }}
              />
              <span className="font-mono">
                {p.toUpperCase()} {avg?.toFixed(1)} ms
                {min !== undefined && max !== undefined
                  ? ` (${min.toFixed(1)}–${max.toFixed(1)})`
                  : ""}
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function LossTooltip(props: TooltipContentProps) {
  const { active, payload, label } = props;
  if (!active || !payload || payload.length === 0 || typeof label !== "string") return null;
  const row = payload[0]?.payload as ChartRow | undefined;
  if (!row) return null;
  return (
    <div
      role="tooltip"
      className="rounded border bg-background/95 p-2 text-xs shadow-md backdrop-blur-sm"
    >
      <div className="mb-1 font-medium">{formatFullTimestamp(label)}</div>
      <ul className="space-y-0.5">
        {CHART_PROTOCOLS.filter((p) => row[`${p}_loss`] !== undefined).map((p) => (
          <li key={p} className="flex items-center gap-2">
            <span
              aria-hidden
              className="inline-block h-2 w-2 rounded-sm"
              style={{ background: PROTOCOL_COLORS[p] }}
            />
            <span className="font-mono">
              {p.toUpperCase()} {row[`${p}_loss`]?.toFixed(2)}%
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
