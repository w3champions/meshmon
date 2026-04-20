import { describe, expect, test } from "vitest";
import { normalizeIpPrefix } from "./ip-prefix";

describe("normalizeIpPrefix", () => {
  test("returns undefined for empty or whitespace input", () => {
    expect(normalizeIpPrefix("")).toBeUndefined();
    expect(normalizeIpPrefix("   ")).toBeUndefined();
  });

  test("passes through CIDR input verbatim (trimmed)", () => {
    expect(normalizeIpPrefix("10.0.0.0/24")).toBe("10.0.0.0/24");
    expect(normalizeIpPrefix("  192.168.1.0/24 ")).toBe("192.168.1.0/24");
  });

  test("expands partial dotted IPv4 to CIDR", () => {
    expect(normalizeIpPrefix("10")).toBe("10.0.0.0/8");
    expect(normalizeIpPrefix("10.0")).toBe("10.0.0.0/16");
    expect(normalizeIpPrefix("10.0.0")).toBe("10.0.0.0/24");
    expect(normalizeIpPrefix("10.0.0.1")).toBe("10.0.0.1/32");
  });

  test("tolerates a trailing dot — the natural operator input", () => {
    expect(normalizeIpPrefix("10.")).toBe("10.0.0.0/8");
    expect(normalizeIpPrefix("10.0.")).toBe("10.0.0.0/16");
    expect(normalizeIpPrefix("10.0.0.")).toBe("10.0.0.0/24");
  });

  test("rejects invalid octets and returns the raw input for backend to handle", () => {
    expect(normalizeIpPrefix("10.256.0.0")).toBe("10.256.0.0");
    expect(normalizeIpPrefix("10.abc")).toBe("10.abc");
    expect(normalizeIpPrefix("10.0.0.0.0")).toBe("10.0.0.0.0");
  });

  test("passes IPv6 input through unchanged", () => {
    expect(normalizeIpPrefix("2001:db8::/32")).toBe("2001:db8::/32");
    expect(normalizeIpPrefix("::1")).toBe("::1");
  });
});
