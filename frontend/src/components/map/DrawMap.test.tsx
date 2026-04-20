import { screen } from "@testing-library/react";
import L from "leaflet";
import { beforeEach, describe, expect, test, vi } from "vitest";
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

// leaflet.markercluster attaches L.MarkerClusterGroup at module load; the
// react-leaflet-cluster wrapper needs a live Leaflet map to mount against.
// Neither is exercisable under jsdom, so stub both.
vi.mock("leaflet.markercluster/dist/MarkerCluster.css", () => ({}));
vi.mock("leaflet.markercluster/dist/MarkerCluster.Default.css", () => ({}));
vi.mock("react-leaflet-cluster", async () => {
  const { MarkerClusterGroupMock } = await import("@/test/leaflet-mock");
  return { default: MarkerClusterGroupMock };
});

import { DrawMap } from "@/components/map/DrawMap";
import { getLeafletMock, resetLeafletMock } from "@/test/leaflet-mock";
import { renderWithProviders } from "@/test/query-wrapper";

describe("DrawMap", () => {
  beforeEach(() => {
    resetLeafletMock();
  });

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

  test("projects pm:create → onShapesChange for rectangle, polygon, and circle", async () => {
    const handler = vi.fn<(next: GeoShape[]) => void>();
    renderWithProviders(<DrawMap shapes={[]} onShapesChange={handler} />);
    await screen.findByTestId("draw-map-shell");

    // Seed real Leaflet layers into the mock map. `instanceof L.Rectangle`
    // / `instanceof L.Polygon` / `instanceof L.Circle` in `layerToShape`
    // resolves against the same Leaflet module, so the duck-typing the
    // component does lines up with real prototype chains.
    const map = getLeafletMock();
    map.__drawnLayers.push(
      L.rectangle([
        [10, 20], // SW lat, lng
        [30, 40], // NE lat, lng
      ]),
      L.polygon([
        [0, 0],
        [0, 5],
        [5, 5],
      ]),
      L.circle([7, 8], { radius: 12_345 }),
    );

    // Geoman would normally raise `pm:create` when the user finishes a
    // shape. Simulate that so the mount-effect's `emit` callback runs.
    map.__fire("pm:create");

    expect(handler).toHaveBeenCalledTimes(1);
    const emitted = handler.mock.calls[0][0];
    expect(emitted).toEqual([
      // Rectangle comes back as (lng, lat) — the component swaps from
      // Leaflet's (lat, lng) to GeoJSON order on its way out.
      { kind: "rectangle", sw: [20, 10], ne: [40, 30] },
      {
        kind: "polygon",
        coordinates: [
          [0, 0],
          [5, 0],
          [5, 5],
        ],
      },
      { kind: "circle", center: [8, 7], radiusMeters: 12_345 },
    ]);
  });

  test("defaults cluster group to zoomToBoundsOnClick when onClusterClick is omitted", async () => {
    const pins = [{ id: "p1", lat: 1, lon: 2 }];
    renderWithProviders(<DrawMap shapes={[]} onShapesChange={() => {}} pins={pins} />);
    const group = await screen.findByTestId("marker-cluster-group");
    expect(group).toHaveAttribute("data-zoom-to-bounds-on-click", "true");
    expect(group).toHaveAttribute("data-has-on-click", "false");
  });

  test("disables zoomToBoundsOnClick and wires onClick when onClusterClick is provided", async () => {
    const pins = [{ id: "p1", lat: 1, lon: 2 }];
    const handler = vi.fn<(ids: string[]) => void>();
    renderWithProviders(
      <DrawMap shapes={[]} onShapesChange={() => {}} pins={pins} onClusterClick={handler} />,
    );
    const group = await screen.findByTestId("marker-cluster-group");
    expect(group).toHaveAttribute("data-zoom-to-bounds-on-click", "false");
    expect(group).toHaveAttribute("data-has-on-click", "true");
  });

  test("pm:edit and pm:remove also flush collected shapes through onShapesChange", async () => {
    const handler = vi.fn<(next: GeoShape[]) => void>();
    renderWithProviders(<DrawMap shapes={[]} onShapesChange={handler} />);
    await screen.findByTestId("draw-map-shell");

    const map = getLeafletMock();
    map.__drawnLayers.push(
      L.rectangle([
        [0, 0],
        [1, 1],
      ]),
    );

    map.__fire("pm:edit");
    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler.mock.calls[0][0]).toEqual([{ kind: "rectangle", sw: [0, 0], ne: [1, 1] }]);

    // A user deletes the shape → geoman fires pm:remove with the layer
    // already pulled from `getGeomanDrawLayers`. The component should
    // emit the now-empty array.
    map.__drawnLayers.length = 0;
    map.__fire("pm:remove");
    expect(handler).toHaveBeenCalledTimes(2);
    expect(handler.mock.calls[1][0]).toEqual([]);
  });
});
