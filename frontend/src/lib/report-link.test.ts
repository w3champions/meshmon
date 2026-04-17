import { describe, expect, test } from "vitest";
import { buildReportPath } from "@/lib/report-link";

describe("buildReportPath", () => {
  test("includes all required params keyed by agent id", () => {
    expect(
      buildReportPath({
        source_id: "fra-core-01",
        target_id: "nyc-core-02",
        from: "2026-04-13T10:15:00Z",
        to: "2026-04-13T14:30:00Z",
        protocol: "icmp",
      }),
    ).toBe(
      "/reports/path?source_id=fra-core-01&target_id=nyc-core-02&from=2026-04-13T10%3A15%3A00Z&to=2026-04-13T14%3A30%3A00Z&protocol=icmp",
    );
  });

  test("omits protocol when not provided", () => {
    expect(
      buildReportPath({
        source_id: "a",
        target_id: "b",
        from: "2026-04-13T10:15:00Z",
        to: "2026-04-13T14:30:00Z",
      }),
    ).toBe(
      "/reports/path?source_id=a&target_id=b&from=2026-04-13T10%3A15%3A00Z&to=2026-04-13T14%3A30%3A00Z",
    );
  });
});
