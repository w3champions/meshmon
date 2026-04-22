import { describe, expect, test } from "vitest";
import {
  formatIpWithHostname,
  hostnameDisplay,
  MAX_HOSTNAME_CHARS,
  tooltipForHostname,
} from "@/components/ip-hostname/format";

describe("formatIpWithHostname", () => {
  test("renders `ip (hostname)` on a positive hit", () => {
    expect(formatIpWithHostname("203.0.113.10", "mail.example.com")).toBe(
      "203.0.113.10 (mail.example.com)",
    );
  });

  test("renders the bare ip when hostname is null (negative cache)", () => {
    expect(formatIpWithHostname("203.0.113.10", null)).toBe("203.0.113.10");
  });

  test("renders the bare ip when hostname is undefined (cold miss)", () => {
    expect(formatIpWithHostname("203.0.113.10", undefined)).toBe("203.0.113.10");
  });

  test("renders the bare ip when hostname is an empty string", () => {
    // Defense in depth — the DTO shouldn't emit `""`, but we don't want a
    // render site to show dangling parens if something regresses upstream.
    expect(formatIpWithHostname("203.0.113.10", "")).toBe("203.0.113.10");
  });

  test("middle-truncates hostnames longer than MAX_HOSTNAME_CHARS", () => {
    // Build a 65-char hostname: `a` * 32 + marker + `b` * 32
    const long = `${"a".repeat(32)}X${"b".repeat(32)}`;
    expect(long.length).toBe(MAX_HOSTNAME_CHARS + 1);

    const formatted = formatIpWithHostname("203.0.113.10", long);
    // Expect `a`×32 + `…` + `b`×32 inside the parens.
    expect(formatted).toBe(`203.0.113.10 (${"a".repeat(32)}…${"b".repeat(32)})`);
    // The ellipsis keeps the display to 32+1+32 chars regardless of input.
    expect(hostnameDisplay(long).length).toBe(65);
  });

  test("leaves hostnames at exactly MAX_HOSTNAME_CHARS untouched", () => {
    const borderline = "a".repeat(MAX_HOSTNAME_CHARS);
    expect(hostnameDisplay(borderline)).toBe(borderline);
    expect(formatIpWithHostname("203.0.113.10", borderline)).toBe(`203.0.113.10 (${borderline})`);
  });

  test("renders IPv6 plainly (no brackets)", () => {
    // Spec §7.2 / default 5: IPv6 is rendered as bare text form, not URL form.
    expect(formatIpWithHostname("2001:db8::1", "v6.example.com")).toBe(
      "2001:db8::1 (v6.example.com)",
    );
    expect(formatIpWithHostname("2001:db8::1", null)).toBe("2001:db8::1");
  });
});

describe("tooltipForHostname", () => {
  test("returns the hostname as-is on a positive hit (untruncated)", () => {
    const long = `${"a".repeat(32)}X${"b".repeat(32)}`;
    expect(tooltipForHostname(long)).toBe(long);
  });

  test("returns undefined for null / undefined / empty string", () => {
    expect(tooltipForHostname(null)).toBeUndefined();
    expect(tooltipForHostname(undefined)).toBeUndefined();
    expect(tooltipForHostname("")).toBeUndefined();
  });
});
