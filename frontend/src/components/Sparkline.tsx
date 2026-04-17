import { Line, LineChart, ResponsiveContainer } from "recharts";
import { cn } from "@/lib/utils";

interface SparklineProps {
  samples: Array<[number, number]>;
  stroke?: string;
  ariaLabel: string;
  className?: string;
}

export function Sparkline({
  samples,
  stroke = "currentColor",
  ariaLabel,
  className,
}: SparklineProps) {
  if (samples.length === 0) {
    return (
      <span
        role="img"
        aria-label={ariaLabel}
        className={cn("text-xs text-muted-foreground", className)}
      >
        n/a
      </span>
    );
  }
  const data = samples.map(([t, v]) => ({ t, v }));
  return (
    <div role="img" aria-label={ariaLabel} className={cn("h-7 w-28", className)}>
      <ResponsiveContainer width="100%" height="100%">
        <LineChart data={data}>
          <Line
            type="monotone"
            dataKey="v"
            stroke={stroke}
            dot={false}
            strokeWidth={1.5}
            isAnimationActive={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
}
