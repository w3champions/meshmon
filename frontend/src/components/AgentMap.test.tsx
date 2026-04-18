import { screen } from "@testing-library/react";
import L from "leaflet";
import { describe, expect, it, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { HealthMatrix } from "@/api/hooks/health-matrix";

vi.mock("react-leaflet", async () => {
  const { LeafletMock } = await import("@/test/leaflet-mock");
  return LeafletMock;
});

import { AgentMap } from "@/components/AgentMap";
import { renderWithProviders } from "@/test/query-wrapper";

const agents: AgentSummary[] = [
  {
    id: "a",
    display_name: "A",
    ip: "10.0.0.1",
    lat: 48.14,
    lon: 11.58,
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: "2026-04-16T11:59:00Z",
  },
  {
    id: "b",
    display_name: "B",
    ip: "10.0.0.2",
    lat: 51.51,
    lon: -0.13,
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: "2026-04-16T11:59:00Z",
  },
  {
    id: "c",
    display_name: "C (no coords)",
    ip: "10.0.0.3",
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: "2026-04-16T11:59:00Z",
  }, // no lat/lon → skipped
];

describe("AgentMap", () => {
  test("renders one marker per agent with lat/lon", async () => {
    renderWithProviders(<AgentMap agents={agents} matrix={new Map() as HealthMatrix} />);
    const markers = await screen.findAllByTestId("marker");
    expect(markers).toHaveLength(2);
    expect(markers[0]).toHaveAttribute("data-lat", "48.14");
    expect(markers[0]).toHaveAttribute("data-lon", "11.58");
    expect(markers[1]).toHaveAttribute("data-lat", "51.51");
    expect(markers[1]).toHaveAttribute("data-lon", "-0.13");
  });

  test("skips agents without lat/lon", async () => {
    renderWithProviders(<AgentMap agents={agents} matrix={new Map() as HealthMatrix} />);
    const markers = await screen.findAllByTestId("marker");
    expect(markers).toHaveLength(2);
  });

  test("popup renders agent id + worst outgoing state", async () => {
    const matrix: HealthMatrix = new Map([
      ["a>b", { source: "a", target: "b", failureRate: 0.3, state: "unreachable" }],
      ["a>c", { source: "a", target: "c", failureRate: 0.1, state: "degraded" }],
    ]);
    renderWithProviders(<AgentMap agents={agents} matrix={matrix} />);
    const popups = await screen.findAllByTestId("popup");
    // Agent "a" has worst outgoing = unreachable → StatusBadge renders "Unreachable"
    expect(popups[0].textContent).toContain("a");
    expect(popups[0].textContent).toContain("Unreachable");
    // Agent "b" has no outgoing entries → falls back to stale
    expect(popups[1].textContent).toContain("b");
  });
});

describe("AgentMap Leaflet icon setup", () => {
  it("patches L.Icon.Default with a bundled iconUrl", () => {
    const defaults = (
      L.Icon.Default.prototype as unknown as {
        options: { iconUrl?: string; iconRetinaUrl?: string; shadowUrl?: string };
      }
    ).options;
    // After Vite resolves the PNG import, iconUrl is an absolute path (e.g.
    // "/node_modules/leaflet/dist/images/marker-icon.png" in the test env, or
    // a hashed "/assets/marker-icon-<hash>.png" in prod). The unpatched default
    // is the bare relative string "marker-icon.png", which breaks under Vite.
    expect(defaults.iconUrl).toBeTruthy();
    expect(defaults.iconUrl).toMatch(/^(https?:\/\/|\/|data:)/);
    expect(defaults.iconRetinaUrl).toMatch(/^(https?:\/\/|\/|data:)/);
    expect(defaults.shadowUrl).toMatch(/^(https?:\/\/|\/|data:)/);
  });
});
