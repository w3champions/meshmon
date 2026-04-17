import { describe, expect, test } from "vitest";
import {
  buildGrafanaSoloUrl,
  MESHMON_PATH_DASHBOARD,
  PANEL_LOSS,
  PANEL_RTT,
  PANEL_STDDEV,
} from "@/lib/grafana-panels";

describe("grafana panel constants", () => {
  test("dashboard uid matches shared contract", () => {
    expect(MESHMON_PATH_DASHBOARD).toBe("meshmon-path");
  });

  test("panel ids match shared contract", () => {
    expect(PANEL_RTT).toBe(1);
    expect(PANEL_LOSS).toBe(2);
    expect(PANEL_STDDEV).toBe(3);
  });
});

describe("buildGrafanaSoloUrl", () => {
  const base = "https://grafana.example.com";
  const common = {
    uid: MESHMON_PATH_DASHBOARD,
    panelId: PANEL_RTT,
    vars: { source: "src-1", target: "tgt-1", protocol: "icmp" },
    from: "now-1h",
    to: "now",
  };

  test("builds d-solo URL with panelId + var-* + kiosk", () => {
    const url = buildGrafanaSoloUrl({ base, ...common });
    expect(url).toContain("/d-solo/meshmon-path?");
    expect(url).toContain("panelId=1");
    expect(url).toContain("var-source=src-1");
    expect(url).toContain("var-target=tgt-1");
    expect(url).toContain("var-protocol=icmp");
    expect(url).toContain("from=now-1h");
    expect(url).toContain("to=now");
    expect(url).toContain("theme=light");
    expect(url.endsWith("&kiosk")).toBe(true);
  });

  test("defaults to light theme when omitted", () => {
    const url = buildGrafanaSoloUrl({ base, ...common });
    expect(url).toContain("theme=light");
  });

  test("accepts dark theme override", () => {
    const url = buildGrafanaSoloUrl({ base, ...common, theme: "dark" });
    expect(url).toContain("theme=dark");
  });

  test("normalizes trailing slash on base URL", () => {
    const withSlash = buildGrafanaSoloUrl({ base: `${base}/`, ...common });
    const withoutSlash = buildGrafanaSoloUrl({ base, ...common });
    expect(withSlash).toBe(withoutSlash);
  });

  test("URL-encodes variable values with special chars", () => {
    const url = buildGrafanaSoloUrl({
      base,
      ...common,
      vars: { source: "a b", target: "x&y", protocol: "icmp" },
    });
    expect(url).toContain("var-source=a+b");
    expect(url).toContain("var-target=x%26y");
  });
});
