import panels from "@grafana/panels.json";

export const GRAFANA_BASE = "/grafana";

export const MESHMON_PATH_DASHBOARD = "meshmon-path" as const;
export const PANEL_RTT = panels["meshmon-path"].panels.rtt as 1;
export const PANEL_LOSS = panels["meshmon-path"].panels.loss as 2;
export const PANEL_STDDEV = panels["meshmon-path"].panels.stddev as 3;

export interface GrafanaSoloUrlOpts {
  uid: string;
  panelId: number;
  vars: Record<string, string>;
  from: string;
  to: string;
  theme?: "light" | "dark";
}

export function buildGrafanaSoloUrl(opts: GrafanaSoloUrlOpts): string {
  const qs = new URLSearchParams();
  qs.set("panelId", String(opts.panelId));
  for (const [k, v] of Object.entries(opts.vars)) qs.set(`var-${k}`, v);
  qs.set("from", opts.from);
  qs.set("to", opts.to);
  qs.set("theme", opts.theme ?? "light");
  return `${GRAFANA_BASE}/d-solo/${opts.uid}?${qs.toString()}&kiosk`;
}
