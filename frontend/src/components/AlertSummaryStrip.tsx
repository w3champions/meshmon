import { Link } from "@tanstack/react-router";
import { useAlertSummary } from "@/api/hooks/alerts";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

interface AlertSummaryStripProps {
  className?: string;
}

export function AlertSummaryStrip({ className }: AlertSummaryStripProps) {
  const { data, isLoading, isError } = useAlertSummary();

  if (isLoading) {
    return <Skeleton className={cn("h-8 w-64", className)} data-testid="alert-summary-skeleton" />;
  }

  if (isError) {
    return (
      <p role="alert" className={cn("text-sm text-destructive", className)}>
        Failed to load alerts
      </p>
    );
  }

  if (data.total === 0) {
    return <p className={cn("text-sm text-muted-foreground", className)}>No active alerts</p>;
  }

  return (
    <div className={cn("flex items-center gap-3 text-sm", className)}>
      {data.critical > 0 && <Badge variant="destructive">{data.critical} critical</Badge>}
      {data.warning > 0 && (
        <Badge
          variant="default"
          className="bg-amber-500/20 text-amber-900 dark:text-amber-100 border-amber-500/30"
        >
          {data.warning} warning
        </Badge>
      )}
      {data.info > 0 && <Badge variant="secondary">{data.info} info</Badge>}
      <Link
        to="/alerts"
        className="underline underline-offset-2 text-muted-foreground hover:text-foreground"
      >
        View all
      </Link>
    </div>
  );
}
