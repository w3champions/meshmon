export interface ReportLinkOpts {
  /** Agent IDs (not IPs) — stable across IP renumbering. */
  source_id: string;
  target_id: string;
  from: string; // ISO-8601
  to: string; // ISO-8601
  protocol?: "icmp" | "udp" | "tcp";
}

/**
 * Build the canonical `/reports/path?...` URL. The URL is self-contained —
 * sharing it reproduces the exact report (no persisted state, no UUIDs).
 *
 * Keyed by agent IDs rather than IPs because agent IPs can change over the
 * agent's lifetime; IDs are stable. The report page still *renders* IPs,
 * but looks them up from the overview response (`source.ip` / `target.ip`).
 *
 * Param order is fixed (`source_id, target_id, from, to, protocol?`) so two
 * calls with the same inputs produce identical strings, keeping shared
 * links stable. `protocol` is optional; when omitted the report defaults
 * to whatever the backend chooses.
 */
export function buildReportPath(opts: ReportLinkOpts): string {
  const qs = new URLSearchParams();
  qs.set("source_id", opts.source_id);
  qs.set("target_id", opts.target_id);
  qs.set("from", opts.from);
  qs.set("to", opts.to);
  if (opts.protocol) qs.set("protocol", opts.protocol);
  return `/reports/path?${qs.toString()}`;
}
