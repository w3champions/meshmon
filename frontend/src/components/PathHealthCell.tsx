import { Link } from "@tanstack/react-router";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import type { HealthState } from "@/lib/health";
import { cn } from "@/lib/utils";

interface PathHealthCellProps {
  source: string;
  target: string;
  state: HealthState;
  failureRate?: number;
}

const COLOUR: Record<HealthState, string> = {
  normal: "bg-emerald-500/80 hover:bg-emerald-500",
  degraded: "bg-amber-400/80 hover:bg-amber-400",
  unreachable: "bg-rose-500/80 hover:bg-rose-500",
  stale: "bg-muted hover:bg-muted/80",
};

function formatRate(rate: number): string {
  return `${(rate * 100).toFixed(1)}%`;
}

export function PathHealthCell({ source, target, state, failureRate }: PathHealthCellProps) {
  const tooltipText =
    state === "stale" || failureRate === undefined
      ? "No data"
      : `${source} → ${target}: ${formatRate(failureRate)}`;

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Link
          // biome-ignore lint/suspicious/noExplicitAny: route not yet in Register
          to={"/paths/$source/$target" as any}
          // biome-ignore lint/suspicious/noExplicitAny: params follow unregistered route
          params={{ source, target } as any}
          className={cn("block h-6 w-6 rounded-sm transition-colors", COLOUR[state])}
          data-state={state}
          aria-label={`${source} to ${target}, ${state}`}
        />
      </TooltipTrigger>
      <TooltipContent>{tooltipText}</TooltipContent>
    </Tooltip>
  );
}
