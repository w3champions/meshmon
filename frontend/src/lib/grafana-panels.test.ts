import { describe, expect, test } from "vitest";
import {
  buildGrafanaSoloUrl,
  GRAFANA_BASE,
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

  test("grafana base is the same-origin proxy mount", () => {
    expect(GRAFANA_BASE).toBe("/grafana");
  });
});

describe("buildGrafanaSoloUrl", () => {
  const common = {
    uid: MESHMON_PATH_DASHBOARD,
    panelId: PANEL_RTT,
    vars: { source: "src-1", target: "tgt-1", protocol: "icmp" },
    from: "now-1h",
    to: "now",
  };

  test("builds a same-origin d-solo URL with panelId + var-* + kiosk", () => {
    const url = buildGrafanaSoloUrl(common);
    expect(url.startsWith("/grafana/d-solo/meshmon-path?")).toBe(true);
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
    const url = buildGrafanaSoloUrl(common);
    expect(url).toContain("theme=light");
  });

  test("accepts dark theme override", () => {
    const url = buildGrafanaSoloUrl({ ...common, theme: "dark" });
    expect(url).toContain("theme=dark");
  });

  test("URL-encodes variable values with special chars", () => {
    const url = buildGrafanaSoloUrl({
      ...common,
      vars: { source: "a b", target: "x&y", protocol: "icmp" },
    });
    expect(url).toContain("var-source=a+b");
    expect(url).toContain("var-target=x%26y");
  });
});
