/**
 * Client-side mirror of the server's `parse::parse_ip_tokens`
 * (see `crates/service/src/catalogue/parse.rs`).
 *
 * Splits operator-pasted input on whitespace, commas, or newlines, and
 * classifies each non-empty token as either an accepted host IP or a
 * rejected token with a stable reason string.
 *
 * Semantics mirrored from the server:
 * - Bare IPv4 / IPv6 addresses are accepted and normalized.
 * - CIDR literals are accepted only when the suffix equals the family's
 *   full host width (`/32` for v4, `/128` for v6); any other suffix is
 *   rejected with `cidr_not_allowed:/<N>`.
 * - Anything that is not a valid IP or a host-width CIDR is rejected
 *   with `invalid_ip`.
 * - Accepted IPs are deduplicated against their canonical form;
 *   `dupeCount` is the total occurrence count across the whole input.
 */

export interface AcceptedEntry {
  ip: string;
  dupeCount: number;
}

export interface RejectedEntry {
  token: string;
  reason: string;
}

export interface ParseOutcome {
  accepted: AcceptedEntry[];
  rejected: RejectedEntry[];
}

const IPV4_RE = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/;
const IPV4_HOST_PREFIX = 32;
const IPV6_HOST_PREFIX = 128;

interface ParsedIp {
  canonical: string;
  family: "v4" | "v6";
}

function parseIPv4(token: string): string | null {
  const match = IPV4_RE.exec(token);
  if (!match) return null;
  const octets: number[] = [];
  for (let i = 1; i <= 4; i += 1) {
    const raw = match[i];
    // Reject leading-zero octets like "01" to match strict IP parsers.
    if (raw.length > 1 && raw.startsWith("0")) return null;
    const value = Number(raw);
    if (!Number.isInteger(value) || value < 0 || value > 255) return null;
    octets.push(value);
  }
  return octets.join(".");
}

function parseIPv6(token: string): string | null {
  // Reject anything containing non-v6 characters outright; `URL` is
  // permissive about some odd inputs, so we gate first.
  if (!/^[0-9A-Fa-f:.]+$/.test(token)) return null;
  try {
    const url = new URL(`http://[${token}]/`);
    const hostname = url.hostname;
    if (!hostname.startsWith("[") || !hostname.endsWith("]")) {
      // `URL` may canonicalize IPv4-mapped forms to bare IPv4; treat
      // those as non-v6 so the caller can fall back to v4 parsing.
      return null;
    }
    return hostname.slice(1, -1);
  } catch {
    return null;
  }
}

function parseIp(token: string): ParsedIp | null {
  const v4 = parseIPv4(token);
  if (v4) return { canonical: v4, family: "v4" };
  const v6 = parseIPv6(token);
  if (v6) return { canonical: v6, family: "v6" };
  return null;
}

interface CidrSplit {
  address: string;
  suffix: string;
}

function splitCidr(token: string): CidrSplit | null {
  const slashIndex = token.indexOf("/");
  if (slashIndex === -1) return null;
  // Reject tokens with more than one slash.
  if (token.indexOf("/", slashIndex + 1) !== -1) return null;
  return {
    address: token.slice(0, slashIndex),
    suffix: token.slice(slashIndex + 1),
  };
}

function parseSuffix(suffix: string): number | null {
  if (suffix.length === 0) return null;
  // Require pure digits — reject "+32", "0x20", "32 ", etc.
  // The `\d+` guard guarantees `Number(suffix)` is a non-negative integer.
  if (!/^\d+$/.test(suffix)) return null;
  return Number(suffix);
}

type Classification =
  | { kind: "accepted"; canonical: string }
  | { kind: "rejected"; reason: string };

function classifyToken(token: string): Classification {
  const cidr = splitCidr(token);
  if (cidr) {
    const prefix = parseSuffix(cidr.suffix);
    const parsed = parseIp(cidr.address);
    if (prefix === null || parsed === null) {
      return { kind: "rejected", reason: "invalid_ip" };
    }
    const hostPrefix = parsed.family === "v4" ? IPV4_HOST_PREFIX : IPV6_HOST_PREFIX;
    if (prefix > hostPrefix) {
      return { kind: "rejected", reason: "invalid_ip" };
    }
    if (prefix !== hostPrefix) {
      return { kind: "rejected", reason: `cidr_not_allowed:/${prefix}` };
    }
    return { kind: "accepted", canonical: parsed.canonical };
  }
  const parsed = parseIp(token);
  if (!parsed) return { kind: "rejected", reason: "invalid_ip" };
  return { kind: "accepted", canonical: parsed.canonical };
}

export function parsePasteInput(input: string): ParseOutcome {
  const tokens = input.split(/[\s,]+/).filter((t) => t.length > 0);

  const acceptedOrder: string[] = [];
  const acceptedCounts = new Map<string, number>();
  const rejected: RejectedEntry[] = [];

  for (const token of tokens) {
    const result = classifyToken(token);
    if (result.kind === "accepted") {
      const existing = acceptedCounts.get(result.canonical);
      if (existing === undefined) {
        acceptedOrder.push(result.canonical);
        acceptedCounts.set(result.canonical, 1);
      } else {
        acceptedCounts.set(result.canonical, existing + 1);
      }
    } else {
      rejected.push({ token, reason: result.reason });
    }
  }

  const accepted: AcceptedEntry[] = acceptedOrder.map((ip) => ({
    ip,
    dupeCount: acceptedCounts.get(ip) ?? 1,
  }));

  return { accepted, rejected };
}
