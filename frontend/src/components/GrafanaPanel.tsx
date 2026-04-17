import { useWebConfig } from "@/api/hooks/web-config";
import { Skeleton } from "@/components/ui/skeleton";
import { buildGrafanaSoloUrl } from "@/lib/grafana-panels";
import { cn } from "@/lib/utils";

interface GrafanaPanelProps {
  dashboard: string;
  panelId: number;
  vars: Record<string, string>;
  from: string;
  to: string;
  title: string;
  className?: string;
  theme?: "light" | "dark";
}

export function GrafanaPanel({
  dashboard,
  panelId,
  vars,
  from,
  to,
  title,
  className,
  theme = "light",
}: GrafanaPanelProps) {
  const { data, isLoading } = useWebConfig();

  if (isLoading) {
    return (
      <Skeleton className={cn("h-56 w-full", className)} data-testid="grafana-panel-skeleton" />
    );
  }

  const base = data?.grafana_base_url;
  if (!base) {
    return (
      <div
        className={cn(
          "flex h-56 w-full items-center justify-center rounded border border-dashed text-sm text-muted-foreground",
          className,
        )}
      >
        Grafana not configured.
      </div>
    );
  }

  const uid = data?.grafana_dashboards?.[dashboard];
  if (!uid) {
    return (
      <div
        className={cn(
          "flex h-56 w-full items-center justify-center rounded border border-dashed text-sm text-muted-foreground",
          className,
        )}
      >
        Dashboard "{dashboard}" not configured.
      </div>
    );
  }

  const src = buildGrafanaSoloUrl({ base, uid, panelId, vars, from, to, theme });
  return (
    <iframe
      title={title}
      src={src}
      className={cn("h-56 w-full rounded border", className)}
      loading="lazy"
      // Don't leak the full path + query (which includes agent IDs) to a
      // cross-origin Grafana when the panel first loads.
      referrerPolicy="no-referrer"
      // Grafana's solo panel needs its own origin (for cookies/API calls) and
      // script execution, but nothing else. Keep the sandbox tight so the
      // embedded page can't navigate the top frame or spawn popups.
      sandbox="allow-same-origin allow-scripts"
    />
  );
}
