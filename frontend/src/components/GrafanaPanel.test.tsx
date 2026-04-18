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

describe("GrafanaPanel stability", () => {
  test("keeps the same <iframe> node when src-affecting props change", () => {
    const { container, rerender } = render(
      <GrafanaPanel
        dashboard="meshmon-path"
        panelId={1}
        vars={{ source: "a", target: "b", protocol: "icmp" }}
        from="now-1h"
        to="now"
        title="RTT"
      />,
    );
    const first = container.querySelector("iframe");
    expect(first).not.toBeNull();
    const firstSrc = first?.getAttribute("src");

    // Change the protocol — this flips the `var-protocol` query param, so the
    // computed src changes. The iframe DOM node itself must be preserved and
    // updated in place via the ref-based effect instead of being torn down
    // and recreated.
    rerender(
      <GrafanaPanel
        dashboard="meshmon-path"
        panelId={1}
        vars={{ source: "a", target: "b", protocol: "udp" }}
        from="now-1h"
        to="now"
        title="RTT"
      />,
    );
    const second = container.querySelector("iframe");
    expect(second).toBe(first);
    // Sanity: the new src actually reflects the changed prop — otherwise the
    // memo comparator would be silently over-eager and block the update.
    expect(second?.getAttribute("src")).not.toBe(firstSrc);
    expect(second?.getAttribute("src")).toContain("var-protocol=udp");
  });

  test("memo blocks re-render when all props are shallow-equal", () => {
    const { container, rerender } = render(
      <GrafanaPanel
        dashboard="meshmon-path"
        panelId={1}
        vars={{ source: "a", target: "b", protocol: "icmp" }}
        from="now-1h"
        to="now"
        title="RTT"
      />,
    );
    const first = container.querySelector("iframe");
    // A fresh `vars` object literal with identical entries must not cause a
    // DOM swap — otherwise every parent render where `vars` is inlined would
    // reload the panel.
    rerender(
      <GrafanaPanel
        dashboard="meshmon-path"
        panelId={1}
        vars={{ source: "a", target: "b", protocol: "icmp" }}
        from="now-1h"
        to="now"
        title="RTT"
      />,
    );
    const second = container.querySelector("iframe");
    expect(second).toBe(first);
  });
});
