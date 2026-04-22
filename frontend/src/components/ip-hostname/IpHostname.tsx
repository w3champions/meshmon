import { hostnameDisplay, tooltipForHostname } from "@/components/ip-hostname/format";
import { useIpHostname } from "@/components/ip-hostname/useIpHostname";

export interface IpHostnameProps {
  /** IPv4 / IPv6 literal in text form — matches the DTO `ip` field. */
  ip: string;
  /**
   * How to render when the provider has no positive hit yet.
   *
   * - `"ip-only"` (default) — render the bare IP so operators still see
   *   something meaningful in tables and header metadata.
   * - `"none"` — render nothing. Useful for composition slots where the
   *   absence of a hostname should collapse the row entirely.
   */
  fallback?: "ip-only" | "none";
}

/**
 * Render an IP + optional reverse-DNS hostname, sourced from the shared
 * `<IpHostnameProvider>`.
 *
 * - Positive hit — renders the IP followed by a muted `(hostname)`
 *   suffix, with the full (untruncated) hostname exposed via the native
 *   `title` attribute so operators can hover-reveal clipped names.
 * - Negative / cold-miss — renders the bare IP (or nothing, per the
 *   `fallback` prop).
 *
 * Accessibility: on a positive hit a visually-hidden `sr-only` span
 * announces `"<ip>, hostname <hostname>"` as a single phrase to screen
 * readers while the visible parenthesised form stays hidden from them
 * via `aria-hidden`. Negative / cold-miss renders leave the bare IP as
 * the announced text — no extra markup.
 *
 * The component applies a single root utility class (`font-mono`) to
 * keep IP columns aligned in tables. Callers that want additional
 * styling wrap the component instead of passing `className` — the
 * module treats bespoke overrides as a smell that leads to drift
 * between render sites.
 */
export function IpHostname({ ip, fallback = "ip-only" }: IpHostnameProps) {
  const hostname = useIpHostname(ip);
  const tooltip = tooltipForHostname(hostname);

  if (typeof hostname === "string" && hostname.length > 0) {
    const announce = `${ip}, hostname ${hostname}`;
    return (
      <span className="font-mono" title={tooltip}>
        <span aria-hidden="true">
          {ip}
          <span className="text-muted-foreground ml-1">({hostnameDisplay(hostname)})</span>
        </span>
        <span className="sr-only">{announce}</span>
      </span>
    );
  }

  if (fallback === "none") return null;
  return <span className="font-mono">{ip}</span>;
}
