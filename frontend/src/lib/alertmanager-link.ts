export const ALERTMANAGER_BASE = "/alertmanager";

export type AlertmanagerLabels = Partial<Record<string, string | undefined>>;

/**
 * Assemble Alertmanager's `#/alerts?filter=...` URL from a label bag.
 * Empty / undefined values are dropped. Returns `null` when no useful
 * matcher would remain so callers can render a non-link span.
 *
 * The filter is a Prometheus-style matcher expression wrapped in braces,
 * e.g. `{alertname="PathPacketLoss",source="brazil-north"}`, URL-encoded
 * as a single query value. Alertmanager is same-origin (proxied at
 * `/alertmanager`) so the base is hardcoded rather than configured.
 */
export function buildAlertmanagerUrl(labels: AlertmanagerLabels): string | null {
  const entries = Object.entries(labels)
    .filter((kv): kv is [string, string] => typeof kv[1] === "string" && kv[1].length > 0)
    .map(([k, v]) => `${k}="${v.replace(/"/g, '\\"')}"`);
  if (entries.length === 0) return null;
  const filter = `{${entries.join(",")}}`;
  return `${ALERTMANAGER_BASE}/#/alerts?filter=${encodeURIComponent(filter)}`;
}
