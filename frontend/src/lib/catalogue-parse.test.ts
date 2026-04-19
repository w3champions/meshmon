import { describe, expect, it } from "vitest";
import { parsePasteInput } from "./catalogue-parse";

describe("parsePasteInput", () => {
  it("accepts two distinct IPv4 addresses separated by whitespace", () => {
    const out = parsePasteInput("1.1.1.1 8.8.8.8");
    expect(out.rejected).toEqual([]);
    expect(out.accepted).toEqual([
      { ip: "1.1.1.1", dupeCount: 1 },
      { ip: "8.8.8.8", dupeCount: 1 },
    ]);
  });

  it("collapses duplicates across comma, whitespace, and newline delimiters", () => {
    const out = parsePasteInput("1.1.1.1, 1.1.1.1\n1.1.1.1");
    expect(out.rejected).toEqual([]);
    expect(out.accepted).toHaveLength(1);
    expect(out.accepted[0]).toEqual({ ip: "1.1.1.1", dupeCount: 3 });
  });

  it("rejects IPv4 CIDR wider than /32 with cidr_not_allowed reason", () => {
    const out = parsePasteInput("10.0.0.0/24");
    expect(out.accepted).toEqual([]);
    expect(out.rejected).toEqual([{ token: "10.0.0.0/24", reason: "cidr_not_allowed:/24" }]);
  });

  it("accepts /32 CIDR as a bare IPv4 host", () => {
    const out = parsePasteInput("1.1.1.1/32");
    expect(out.rejected).toEqual([]);
    expect(out.accepted).toEqual([{ ip: "1.1.1.1", dupeCount: 1 }]);
  });

  it("rejects garbage tokens with invalid_ip reason", () => {
    const out = parsePasteInput("not-an-ip");
    expect(out.accepted).toEqual([]);
    expect(out.rejected).toEqual([{ token: "not-an-ip", reason: "invalid_ip" }]);
  });

  it("accepts IPv6 loopback ::1 and normalizes case", () => {
    const out = parsePasteInput("::1");
    expect(out.rejected).toEqual([]);
    expect(out.accepted).toEqual([{ ip: "::1", dupeCount: 1 }]);
  });

  it("accepts IPv6 /128 as a bare host and rejects wider v6 CIDRs", () => {
    const accepted = parsePasteInput("2001:db8::/128");
    expect(accepted.rejected).toEqual([]);
    expect(accepted.accepted).toEqual([{ ip: "2001:db8::", dupeCount: 1 }]);

    const rejected = parsePasteInput("2001:db8::/48");
    expect(rejected.accepted).toEqual([]);
    expect(rejected.rejected).toEqual([{ token: "2001:db8::/48", reason: "cidr_not_allowed:/48" }]);
  });

  it("normalizes IPv6 case so 2001:DB8::1 and 2001:db8::1 dedupe", () => {
    const out = parsePasteInput("2001:DB8::1 2001:db8::1");
    expect(out.rejected).toEqual([]);
    expect(out.accepted).toEqual([{ ip: "2001:db8::1", dupeCount: 2 }]);
  });

  it("ignores empty tokens between delimiters", () => {
    const out = parsePasteInput(",, ,\n\n1.2.3.4,\t");
    expect(out.rejected).toEqual([]);
    expect(out.accepted).toEqual([{ ip: "1.2.3.4", dupeCount: 1 }]);
  });

  it("mixes accepted and rejected tokens", () => {
    const out = parsePasteInput("1.1.1.1 garbage 10.0.0.0/8 ::1");
    expect(out.accepted).toEqual([
      { ip: "1.1.1.1", dupeCount: 1 },
      { ip: "::1", dupeCount: 1 },
    ]);
    expect(out.rejected).toEqual([
      { token: "garbage", reason: "invalid_ip" },
      { token: "10.0.0.0/8", reason: "cidr_not_allowed:/8" },
    ]);
  });

  it("rejects IPv4 octets out of range", () => {
    const out = parsePasteInput("256.0.0.1");
    expect(out.accepted).toEqual([]);
    expect(out.rejected).toEqual([{ token: "256.0.0.1", reason: "invalid_ip" }]);
  });

  it("rejects CIDR tokens with non-integer suffixes as invalid_ip", () => {
    const out = parsePasteInput("1.1.1.1/abc");
    expect(out.accepted).toEqual([]);
    expect(out.rejected).toEqual([{ token: "1.1.1.1/abc", reason: "invalid_ip" }]);
  });
});
