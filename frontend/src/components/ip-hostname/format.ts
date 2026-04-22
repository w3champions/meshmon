/**
 * Hostname display helpers shared between `<IpHostname>` and any caller that
 * needs a plain-text rendition (e.g. `aria-label`, node labels in a graph
 * canvas, CSV exports).
 *
 * Long reverse-DNS hostnames (> {@link MAX_HOSTNAME_CHARS} characters) are
 * middle-truncated with a single `…` so table cells stay readable. Callers
 * that want the untruncated value for a tooltip use
 * {@link tooltipForHostname}.
 *
 * IPv6 literals are rendered without brackets — the spec default is bare
 * text form (`2001:db8::1 (example.com)`), not the URL form
 * (`[2001:db8::1]:443`).
 */

/** Middle-truncation kicks in once the hostname exceeds this many chars. */
export const MAX_HOSTNAME_CHARS = 64;

/** Characters kept from each end of a truncated hostname (before/after `…`). */
const TRUNCATE_PREFIX = 32;
const TRUNCATE_SUFFIX = 32;

/**
 * Middle-truncate a reverse-DNS hostname that would clip typical table cells.
 *
 * Returns the input unchanged when it fits in {@link MAX_HOSTNAME_CHARS}; the
 * full value is always available via {@link tooltipForHostname} for callers
 * that want to render it inside a `title` attribute.
 */
export function hostnameDisplay(hostname: string): string {
  if (hostname.length <= MAX_HOSTNAME_CHARS) return hostname;
  const head = hostname.slice(0, TRUNCATE_PREFIX);
  const tail = hostname.slice(hostname.length - TRUNCATE_SUFFIX);
  return `${head}…${tail}`;
}

/**
 * Format an IP with an optional hostname suffix.
 *
 * - `null` / `undefined` / empty string → just the IP literal.
 * - Positive hit → `"<ip> (<hostname-display>)"`, middle-truncating long
 *   hostnames via {@link hostnameDisplay}.
 *
 * Deliberately accepts a broad hostname type (`string | null | undefined`) so
 * callers can pipe the provider map value straight through without narrowing.
 */
export function formatIpWithHostname(ip: string, hostname: string | null | undefined): string {
  if (typeof hostname !== "string" || hostname.length === 0) return ip;
  return `${ip} (${hostnameDisplay(hostname)})`;
}

/**
 * Return the untruncated hostname suitable for a `title` tooltip.
 *
 * Separated from the primary display helper so render sites that already
 * pass a possibly-`null` provider map value don't have to branch before
 * assigning `title={...}`; returns `undefined` for non-strings / empty
 * strings so React drops the attribute cleanly.
 */
export function tooltipForHostname(hostname: string | null | undefined): string | undefined {
  if (typeof hostname !== "string" || hostname.length === 0) return undefined;
  return hostname;
}
