import { formatDistanceToNowStrict } from "date-fns";
import type { AlertSummary } from "@/api/hooks/alerts";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { buildAlertmanagerUrl } from "@/lib/alertmanager-link";
import { cn } from "@/lib/utils";

interface AlertRowProps {
  alert: AlertSummary;
  className?: string;
}

const SEVERITY_VARIANT: Record<string, "destructive" | "default" | "secondary"> = {
  critical: "destructive",
  warning: "default",
  info: "secondary",
};

export function AlertRow({ alert, className }: AlertRowProps) {
  const labels = alert.labels ?? {};
  const name = labels.alertname ?? "(unnamed alert)";
  const severity = labels.severity ?? "info";
  const href = buildAlertmanagerUrl({
    alertname: labels.alertname,
    source: labels.source,
    target: labels.target,
  });

  return (
    <Card className={cn("", className)}>
      <CardHeader>
        <div className="flex flex-wrap items-center gap-3">
          <h3 className="text-base font-semibold leading-none">{name}</h3>
          <Badge variant={SEVERITY_VARIANT[severity] ?? "secondary"}>{severity}</Badge>
          <span className="text-xs text-muted-foreground" title={alert.starts_at}>
            started{" "}
            {formatDistanceToNowStrict(new Date(alert.starts_at), {
              addSuffix: true,
            })}
          </span>
        </div>
      </CardHeader>
      <CardContent>
        <div className="flex flex-wrap items-center gap-2 text-xs">
          {labels.source && (
            <Badge variant="outline">
              <span className="mr-1 text-muted-foreground">source:</span>
              {labels.source}
            </Badge>
          )}
          {labels.target && (
            <Badge variant="outline">
              <span className="mr-1 text-muted-foreground">target:</span>
              {labels.target}
            </Badge>
          )}
          {labels.protocol && (
            <Badge variant="outline">
              <span className="mr-1 text-muted-foreground">proto:</span>
              {labels.protocol}
            </Badge>
          )}
          {labels.category && (
            <Badge variant="outline">
              <span className="mr-1 text-muted-foreground">category:</span>
              {labels.category}
            </Badge>
          )}
        </div>
        {alert.summary && <p className="mt-2 text-sm">{alert.summary}</p>}
        {alert.description && (
          <p className="mt-1 text-sm text-muted-foreground">{alert.description}</p>
        )}
        {href && (
          <a
            href={href}
            target="_blank"
            rel="noopener noreferrer"
            className="mt-2 inline-block text-xs underline underline-offset-2"
          >
            View in Alertmanager ↗
          </a>
        )}
      </CardContent>
    </Card>
  );
}
