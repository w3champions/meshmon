import type { AlertSummary } from "@/api/hooks/alerts";

export type Severity = "all" | "critical" | "warning" | "info";
export type ProtocolFilter = "all" | "icmp" | "udp" | "tcp";

export interface AlertFilter {
  severity: Severity;
  /** Concrete category label or "all" for no filter. */
  category: string;
  /** Concrete protocol label or "all" for no filter. */
  protocol: ProtocolFilter;
  /** Substring match against `labels.source` (case-insensitive). */
  source: string;
  /** Substring match against `labels.target` (case-insensitive). */
  target: string;
  /** Substring match across alertname / summary / description. */
  text: string;
}

export function defaultAlertFilter(): AlertFilter {
  return {
    severity: "all",
    category: "all",
    protocol: "all",
    source: "",
    target: "",
    text: "",
  };
}

function containsI(haystack: string | null | undefined, needle: string): boolean {
  if (!needle) return true;
  if (!haystack) return false;
  return haystack.toLowerCase().includes(needle.toLowerCase());
}

export function filterAlerts(alerts: AlertSummary[], filter: AlertFilter): AlertSummary[] {
  return alerts.filter((a) => {
    const labels = a.labels ?? {};
    if (filter.severity !== "all" && labels.severity !== filter.severity) return false;
    if (filter.category !== "all" && labels.category !== filter.category) return false;
    if (filter.protocol !== "all" && labels.protocol !== filter.protocol) return false;
    if (!containsI(labels.source, filter.source)) return false;
    if (!containsI(labels.target, filter.target)) return false;
    if (filter.text) {
      const hay = [labels.alertname ?? "", a.summary ?? "", a.description ?? ""].join(" ");
      if (!containsI(hay, filter.text)) return false;
    }
    return true;
  });
}

export function uniqueCategories(alerts: AlertSummary[]): string[] {
  const set = new Set<string>();
  for (const a of alerts) {
    const c = a.labels?.category;
    if (typeof c === "string" && c.length > 0) set.add(c);
  }
  return [...set].sort();
}
