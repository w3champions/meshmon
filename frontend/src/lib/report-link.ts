export interface ReportLinkOpts {
  source_ip: string;
  target_ip: string;
  from: string; // ISO-8601
  to: string; // ISO-8601
  protocol?: "icmp" | "udp" | "tcp";
}

/**
 * Build the canonical `/reports/path?...` URL. The URL is self-contained —
 * sharing it reproduces the exact report (no persisted state, no UUIDs).
 *
 * Param order is fixed (`source_ip, target_ip, from, to, protocol?`) so two
 * calls with the same inputs produce identical strings, keeping shared
 * links stable. `protocol` is optional; when omitted the report defaults
 * to whatever the backend chooses.
 */
export function buildReportPath(opts: ReportLinkOpts): string {
  const qs = new URLSearchParams();
  qs.set("source_ip", opts.source_ip);
  qs.set("target_ip", opts.target_ip);
  qs.set("from", opts.from);
  qs.set("to", opts.to);
  if (opts.protocol) qs.set("protocol", opts.protocol);
  return `/reports/path?${qs.toString()}`;
}
