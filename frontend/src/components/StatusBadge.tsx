import { Badge } from "@/components/ui/badge";
import type { HealthState } from "@/lib/health";
import { cn } from "@/lib/utils";

export type StatusState = HealthState | "online";

interface StatusBadgeProps {
  state: StatusState;
  className?: string;
}

interface BadgeConfig {
  label: string;
  variant: "default" | "secondary" | "destructive" | "outline";
  extraClass?: string;
}

function getBadgeConfig(state: StatusState): BadgeConfig {
  switch (state) {
    case "online":
      return {
        label: "Online",
        variant: "default",
        extraClass:
          "bg-emerald-500/20 text-emerald-900 dark:text-emerald-100 border-emerald-500/30",
      };
    case "stale":
      return {
        label: "Stale",
        variant: "secondary",
      };
    case "degraded":
      return {
        label: "Degraded",
        variant: "default",
        extraClass: "bg-amber-500/20 text-amber-900 dark:text-amber-100 border-amber-500/30",
      };
    case "unreachable":
      return {
        label: "Unreachable",
        variant: "destructive",
      };
    case "normal":
      return {
        label: "Online",
        variant: "default",
        extraClass:
          "bg-emerald-500/20 text-emerald-900 dark:text-emerald-100 border-emerald-500/30",
      };
  }
}

export function StatusBadge({ state, className }: StatusBadgeProps) {
  const { label, variant, extraClass } = getBadgeConfig(state);
  return (
    <Badge variant={variant} className={cn(extraClass, className)}>
      {label}
    </Badge>
  );
}
