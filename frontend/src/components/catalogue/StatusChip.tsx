import type { components } from "@/api/schema.gen";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

export type EnrichmentStatus = components["schemas"]["EnrichmentStatus"];

export interface StatusChipProps {
  /** Current enrichment pipeline status for the row. */
  status: EnrichmentStatus;
  /** Handler fired when the chip is clicked. Ignored while `status === "pending"`. */
  onReenrich?: () => void;
}

interface ChipConfig {
  label: string;
  variant: "default" | "secondary" | "destructive" | "outline";
  extraClass?: string;
}

function getChipConfig(status: EnrichmentStatus): ChipConfig {
  switch (status) {
    case "enriched":
      return {
        label: "Enriched",
        variant: "default",
        extraClass:
          "bg-emerald-500/20 text-emerald-900 dark:text-emerald-100 border-emerald-500/30",
      };
    case "pending":
      return {
        label: "Pending",
        variant: "secondary",
      };
    case "failed":
      return {
        label: "Failed",
        variant: "destructive",
      };
  }
}

/**
 * Renders a compact chip describing the enrichment status of a catalogue row.
 *
 * When `status === "pending"`, the chip is non-actionable per the T43 scope
 * note ("already queued") and the optional `onReenrich` handler is ignored.
 * For `enriched` and `failed` statuses the chip becomes a button that fires
 * `onReenrich` when clicked.
 */
export function StatusChip({ status, onReenrich }: StatusChipProps) {
  const { label, variant, extraClass } = getChipConfig(status);
  const isPending = status === "pending";
  const isClickable = !isPending && typeof onReenrich === "function";
  const title = isPending ? "already queued" : "Re-enrich";

  return (
    <Badge
      variant={variant}
      className={cn(extraClass, isClickable && "cursor-pointer", isPending && "cursor-default")}
      role={isClickable ? "button" : undefined}
      aria-label={isClickable ? `Re-enrich (${label})` : label}
      tabIndex={isClickable ? 0 : undefined}
      title={title}
      onClick={isClickable ? () => onReenrich?.() : undefined}
      onKeyDown={
        isClickable
          ? (event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                onReenrich?.();
              }
            }
          : undefined
      }
    >
      {label}
    </Badge>
  );
}
