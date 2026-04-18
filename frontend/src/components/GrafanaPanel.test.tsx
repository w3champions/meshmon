import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { GrafanaPanel } from "@/components/GrafanaPanel";

describe("GrafanaPanel", () => {
  test("renders an iframe with a same-origin /grafana d-solo URL", () => {
    render(
      <GrafanaPanel
        dashboard="meshmon-path"
        panelId={1}
        vars={{ source: "a", target: "b", protocol: "icmp" }}
        from="now-1h"
        to="now"
        title="RTT"
      />,
    );
    const iframe = screen.getByTitle("RTT");
    expect(iframe.getAttribute("src")).toBe(
      "/grafana/d-solo/meshmon-path?panelId=1&var-source=a&var-target=b&var-protocol=icmp&from=now-1h&to=now&theme=light&kiosk",
    );
    // The iframe is sandboxed (no top-frame navigation, no popups) and
    // doesn't leak the page URL (which contains agent IDs) as a referrer.
    expect(iframe).toHaveAttribute("sandbox", "allow-same-origin allow-scripts");
    expect(iframe).toHaveAttribute("referrerpolicy", "no-referrer");
  });
});
