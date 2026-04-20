import { screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import type { GeoShape } from "@/lib/geo";

vi.mock("react-leaflet", async () => {
  const { LeafletMock } = await import("@/test/leaflet-mock");
  return LeafletMock;
});

// The @geoman-io/leaflet-geoman-free bundle is an IIFE that attaches to a
// global `L`. It never runs under jsdom (we don't render a real Leaflet
// map), so stub it out with an empty side-effect module.
vi.mock("@geoman-io/leaflet-geoman-free", () => ({}));
vi.mock("@geoman-io/leaflet-geoman-free/dist/leaflet-geoman.css", () => ({}));

import { DrawMap } from "@/components/map/DrawMap";
import { renderWithProviders } from "@/test/query-wrapper";

describe("DrawMap", () => {
  test("mounts without throwing when no shapes or pins are provided", async () => {
    renderWithProviders(<DrawMap shapes={[]} onShapesChange={() => {}} />);
    expect(await screen.findByTestId("draw-map-shell")).toBeInTheDocument();
    expect(screen.getByTestId("map-container")).toBeInTheDocument();
    expect(screen.getByTestId("tile-layer")).toBeInTheDocument();
  });

  test("renders one <Marker> per pin entry", async () => {
    const pins = [
      { id: "p1", lat: 48.14, lon: 11.58 },
      { id: "p2", lat: 51.51, lon: -0.13 },
    ];
    renderWithProviders(<DrawMap shapes={[]} onShapesChange={() => {}} pins={pins} />);
    const markers = await screen.findAllByTestId("marker");
    expect(markers).toHaveLength(2);
    expect(markers[0]).toHaveAttribute("data-lat", "48.14");
    expect(markers[0]).toHaveAttribute("data-lon", "11.58");
    expect(markers[1]).toHaveAttribute("data-lat", "51.51");
    expect(markers[1]).toHaveAttribute("data-lon", "-0.13");
  });

  test("renders a popup when a pin supplies one", async () => {
    const pins = [
      { id: "p1", lat: 1, lon: 2, popup: <span>hello pin</span> },
      { id: "p2", lat: 3, lon: 4 },
    ];
    renderWithProviders(<DrawMap shapes={[]} onShapesChange={() => {}} pins={pins} />);
    const popups = await screen.findAllByTestId("popup");
    // Only the first pin requested a popup.
    expect(popups).toHaveLength(1);
    expect(popups[0].textContent).toBe("hello pin");
  });

  test("accepts a typed GeoShape[] without type error and survives unmount", async () => {
    const shapes: GeoShape[] = [
      { kind: "rectangle", sw: [-10, -10], ne: [10, 10] },
      { kind: "circle", center: [0, 0], radiusMeters: 50_000 },
      {
        kind: "polygon",
        coordinates: [
          [0, 0],
          [1, 0],
          [1, 1],
        ],
      },
    ];
    const handler = vi.fn<(next: GeoShape[]) => void>();
    const { unmount } = renderWithProviders(<DrawMap shapes={shapes} onShapesChange={handler} />);
    expect(await screen.findByTestId("draw-map-shell")).toBeInTheDocument();
    // Clean teardown: no listener leaks, no thrown errors from the
    // GeomanController cleanup effect.
    expect(() => unmount()).not.toThrow();
  });
});
