/**
 * Normalise a user-entered IP prefix into a CIDR string the backend can parse.
 *
 * The catalogue list endpoint parses `ip_prefix` strictly as an `IpNetwork`
 * and silently drops the filter on parse failure. That makes the natural
 * operator input (`10.0.0.`) effectively a no-op. Expand partial dotted IPv4
 * prefixes to canonical CIDR here so the backend sees a valid network.
 *
 * Rules:
 * - Empty / whitespace → undefined (no filter).
 * - Contains `/` → assumed CIDR, passed through trimmed.
 * - Contains `:` → assumed IPv6, passed through trimmed (IPv6 prefix
 *   expansion is not attempted — the backend accepts valid IPv6 CIDRs).
 * - 1–4 dot-separated decimal octets (0–255), with or without a trailing
 *   dot → padded with `.0` to four octets, mask = 8 × octet count.
 *   Examples: `10` → `10.0.0.0/8`, `10.0.` → `10.0.0.0/16`,
 *   `10.0.0.1` → `10.0.0.1/32`.
 * - Anything else → trimmed input, let the backend decide.
 */
export function normalizeIpPrefix(raw: string): string | undefined {
  const trimmed = raw.trim();
  if (trimmed === "") return undefined;
  if (trimmed.includes("/")) return trimmed;
  if (trimmed.includes(":")) return trimmed;

  const stripped = trimmed.replace(/\.$/, "");
  const parts = stripped.split(".");
  if (parts.length < 1 || parts.length > 4) return trimmed;

  const validOctet = (p: string): boolean => /^\d{1,3}$/.test(p) && Number(p) <= 255;
  if (!parts.every(validOctet)) return trimmed;

  const maskBits = parts.length * 8;
  const padded = [...parts, "0", "0", "0", "0"].slice(0, 4);
  return `${padded.join(".")}/${maskBits}`;
}
