import { buildGrafanaSoloUrl } from "@/lib/grafana-panels";
import { cn } from "@/lib/utils";

interface GrafanaPanelProps {
  /** Dashboard UID — matches the Grafana dashboard's `uid` (e.g., `"meshmon-path"`).
   *  Grafana proxies are mounted at `/grafana`; the panel builds `/grafana/d-solo/<uid>?…`. */
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
  const src = buildGrafanaSoloUrl({ uid: dashboard, panelId, vars, from, to, theme });
  return (
    <iframe
      title={title}
      src={src}
      className={cn("h-56 w-full rounded border", className)}
      loading="lazy"
      // Don't leak the full path + query (which includes agent IDs) to the
      // embedded Grafana when the panel first loads. Same-origin now, but
      // the policy still blocks leaking agent IDs into a Grafana access log.
      referrerPolicy="no-referrer"
      // Grafana's solo panel needs same-origin access (for its own cookies /
      // API calls) and script execution, but nothing else. Keep the sandbox
      // tight so the embedded page can't navigate the top frame or spawn
      // popups.
      sandbox="allow-same-origin allow-scripts"
    />
  );
}
