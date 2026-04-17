import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { GrafanaPanel } from "@/components/GrafanaPanel";

afterEach(() => vi.restoreAllMocks());

function wrap(children: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
}

describe("GrafanaPanel", () => {
  test("renders an iframe with a d-solo URL when Grafana is configured", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      async () =>
        new Response(
          JSON.stringify({
            version: "0.1.0",
            username: "u",
            grafana_base_url: "https://grafana.example/",
            grafana_dashboards: { "meshmon-path": "abc123" },
          }),
          { status: 200 },
        ),
    );
    render(
      wrap(
        <GrafanaPanel
          dashboard="meshmon-path"
          panelId={1}
          vars={{ source: "a", target: "b", protocol: "icmp" }}
          from="now-1h"
          to="now"
          title="RTT"
        />,
      ),
    );
    const iframe = await screen.findByTitle("RTT");
    expect(iframe.getAttribute("src")).toBe(
      "https://grafana.example/d-solo/abc123?panelId=1&var-source=a&var-target=b&var-protocol=icmp&from=now-1h&to=now&theme=light&kiosk",
    );
    // The iframe is sandboxed (no top-frame navigation, no popups) and
    // doesn't leak the page URL (which contains agent IDs) as a referrer.
    expect(iframe).toHaveAttribute("sandbox", "allow-same-origin allow-scripts");
    expect(iframe).toHaveAttribute("referrerpolicy", "no-referrer");
  });

  test("shows a fallback when Grafana is unconfigured (no base URL)", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      async () =>
        new Response(JSON.stringify({ version: "0.1.0", username: "u", grafana_dashboards: {} }), {
          status: 200,
        }),
    );
    render(
      wrap(
        <GrafanaPanel
          dashboard="meshmon-path"
          panelId={1}
          vars={{}}
          from="now-1h"
          to="now"
          title="RTT"
        />,
      ),
    );
    expect(await screen.findByText(/grafana not configured/i)).toBeInTheDocument();
  });

  test("shows a fallback when the dashboard UID is missing", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      async () =>
        new Response(
          JSON.stringify({
            version: "0.1.0",
            username: "u",
            grafana_base_url: "https://grafana.example/",
            grafana_dashboards: {},
          }),
          { status: 200 },
        ),
    );
    render(
      wrap(
        <GrafanaPanel
          dashboard="meshmon-path"
          panelId={1}
          vars={{}}
          from="now-1h"
          to="now"
          title="RTT"
        />,
      ),
    );
    expect(await screen.findByText(/dashboard "meshmon-path" not configured/i)).toBeInTheDocument();
  });
});
