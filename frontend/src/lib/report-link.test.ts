import { describe, expect, test } from "vitest";
import { buildReportPathUrl } from "@/lib/report-link";

describe("buildReportPathUrl", () => {
  const base = {
    sourceIp: "170.80.110.90",
    targetIp: "85.90.216.7",
    from: "2026-04-13T10:15:00Z",
    to: "2026-04-13T14:30:00Z",
    protocol: "icmp" as const,
  };

  test("builds a /reports/path URL with all five query params", () => {
    const url = buildReportPathUrl(base);
    expect(url.startsWith("/reports/path?")).toBe(true);
    expect(url).toContain("source_ip=170.80.110.90");
    expect(url).toContain("target_ip=85.90.216.7");
    expect(url).toContain("from=2026-04-13T10%3A15%3A00Z");
    expect(url).toContain("to=2026-04-13T14%3A30%3A00Z");
    expect(url).toContain("protocol=icmp");
  });

  test("supports all three protocols", () => {
    expect(buildReportPathUrl({ ...base, protocol: "icmp" })).toContain("protocol=icmp");
    expect(buildReportPathUrl({ ...base, protocol: "udp" })).toContain("protocol=udp");
    expect(buildReportPathUrl({ ...base, protocol: "tcp" })).toContain("protocol=tcp");
  });

  test("URL-encodes IPv6 addresses safely", () => {
    const url = buildReportPathUrl({
      ...base,
      sourceIp: "2001:db8::1",
      targetIp: "fe80::1",
    });
    expect(url).toContain("source_ip=2001%3Adb8%3A%3A1");
    expect(url).toContain("target_ip=fe80%3A%3A1");
  });

  test("emits params in a stable order for deterministic sharing", () => {
    const urlA = buildReportPathUrl(base);
    const urlB = buildReportPathUrl(base);
    expect(urlA).toBe(urlB);
  });
});
