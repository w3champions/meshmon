import { describe, expect, test } from "vitest";
import { buildReportPath } from "@/lib/report-link";

describe("buildReportPath", () => {
  test("includes all required params", () => {
    expect(
      buildReportPath({
        source_ip: "170.80.110.90",
        target_ip: "85.90.216.7",
        from: "2026-04-13T10:15:00Z",
        to: "2026-04-13T14:30:00Z",
        protocol: "icmp",
      }),
    ).toBe(
      "/reports/path?source_ip=170.80.110.90&target_ip=85.90.216.7&from=2026-04-13T10%3A15%3A00Z&to=2026-04-13T14%3A30%3A00Z&protocol=icmp",
    );
  });

  test("omits protocol when not provided", () => {
    expect(
      buildReportPath({
        source_ip: "1.1.1.1",
        target_ip: "2.2.2.2",
        from: "2026-04-13T10:15:00Z",
        to: "2026-04-13T14:30:00Z",
      }),
    ).toBe(
      "/reports/path?source_ip=1.1.1.1&target_ip=2.2.2.2&from=2026-04-13T10%3A15%3A00Z&to=2026-04-13T14%3A30%3A00Z",
    );
  });
});
