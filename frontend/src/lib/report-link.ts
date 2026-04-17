export type ReportProtocol = "icmp" | "udp" | "tcp";

export interface ReportPathUrlOpts {
  sourceIp: string;
  targetIp: string;
  from: string;
  to: string;
  protocol: ReportProtocol;
}

/**
 * Build the canonical `/reports/path?...` URL. The URL is self-contained —
 * sharing it reproduces the exact report (no persisted state, no UUIDs).
 *
 * Param order is fixed (`source_ip, target_ip, from, to, protocol`) so two
 * calls with the same inputs produce identical strings, keeping shared
 * links stable.
 */
export function buildReportPathUrl(opts: ReportPathUrlOpts): string {
  const qs = new URLSearchParams();
  qs.set("source_ip", opts.sourceIp);
  qs.set("target_ip", opts.targetIp);
  qs.set("from", opts.from);
  qs.set("to", opts.to);
  qs.set("protocol", opts.protocol);
  return `/reports/path?${qs.toString()}`;
}
