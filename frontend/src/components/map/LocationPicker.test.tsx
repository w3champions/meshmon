import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, test, vi } from "vitest";

vi.mock("react-leaflet", async () => {
  const { LeafletMock } = await import("@/test/leaflet-mock");
  return LeafletMock;
});

import { LocationPicker } from "@/components/map/LocationPicker";
import {
  fireMapClick,
  fireMarkerDragEnd,
  getLeafletMock,
  resetLeafletMock,
} from "@/test/leaflet-mock";
import { renderWithProviders } from "@/test/query-wrapper";

describe("LocationPicker", () => {
  beforeEach(() => {
    resetLeafletMock();
  });

  test("renders no marker when value is null and shows empty-state readout", async () => {
    renderWithProviders(<LocationPicker value={null} onChange={() => {}} />);
    expect(await screen.findByTestId("map-container")).toBeInTheDocument();
    expect(screen.queryByTestId("marker")).toBeNull();
    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(status.textContent).toMatch(/no location/i);
  });

  test("renders a marker at the supplied coordinates", async () => {
    renderWithProviders(
      <LocationPicker value={{ latitude: 48.14, longitude: 11.58 }} onChange={() => {}} />,
    );
    const marker = await screen.findByTestId("marker");
    expect(marker).toHaveAttribute("data-lat", "48.14");
    expect(marker).toHaveAttribute("data-lon", "11.58");
    const status = screen.getByRole("status");
    expect(status.textContent).toMatch(/48\.14/);
    expect(status.textContent).toMatch(/11\.58/);
  });

  test("a map click fires onChange with the latlng", async () => {
    const handler = vi.fn();
    renderWithProviders(<LocationPicker value={null} onChange={handler} />);
    await screen.findByTestId("map-container");

    fireMapClick(12.34, -56.78);

    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith({ latitude: 12.34, longitude: -56.78 });
  });

  test("dragging the marker fires onChange with the new position", async () => {
    const handler = vi.fn();
    renderWithProviders(
      <LocationPicker value={{ latitude: 1, longitude: 2 }} onChange={handler} />,
    );
    await screen.findByTestId("marker");

    fireMarkerDragEnd(10.5, 20.25);

    expect(handler).toHaveBeenCalledWith({ latitude: 10.5, longitude: 20.25 });
  });

  test("clicking Clear fires onChange(null) and is keyboard-operable", async () => {
    const handler = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(
      <LocationPicker value={{ latitude: 1, longitude: 2 }} onChange={handler} />,
    );
    const clear = await screen.findByRole("button", { name: /clear location/i });
    await user.click(clear);
    expect(handler).toHaveBeenLastCalledWith(null);

    handler.mockClear();
    // Keyboard path: Enter activates the same button.
    clear.focus();
    await user.keyboard("{Enter}");
    expect(handler).toHaveBeenLastCalledWith(null);
  });

  test("Clear is disabled (or hidden) when no location is set", async () => {
    renderWithProviders(<LocationPicker value={null} onChange={() => {}} />);
    const clear = screen.queryByRole("button", { name: /clear location/i });
    // Either the button is absent, or it is disabled. Both are acceptable —
    // they both prevent a no-op onChange(null) call.
    if (clear) {
      expect(clear).toBeDisabled();
    }
  });

  test("mounting with a supplied value recenters from world → continent-level", async () => {
    // First render frames the operator at the world overview
    // (DEFAULT_CENTER / DEFAULT_ZOOM) and only then animates in to
    // the supplied point at FIRST_PICK_ZOOM. This keeps the visible
    // mount framing identical to the value=null case and lets the
    // post-mount `setView` carry the meaningful transition.
    renderWithProviders(
      <LocationPicker value={{ latitude: 10, longitude: 20 }} onChange={() => {}} />,
    );
    await screen.findByTestId("map-container");
    const map = getLeafletMock();
    const last = map.__setViewCalls.at(-1);
    expect(last?.center).toEqual([10, 20]);
    expect(last?.zoom).toBe(4);
  });

  test("plain wheel bubbles (page scrolls) and flashes the modifier hint", async () => {
    renderWithProviders(<LocationPicker value={null} onChange={() => {}} />);
    await screen.findByTestId("map-container");
    const map = getLeafletMock();
    const container = map.getContainer();
    const beforeZoom = map.getZoom();

    const hint = screen.getByTestId("zoom-hint");
    expect(hint.className).toMatch(/opacity-0/);

    container.dispatchEvent(new WheelEvent("wheel", { deltaY: 100, bubbles: true }));

    // Plain wheel does not zoom the map — the browser is free to scroll
    // the page instead.
    expect(map.getZoom()).toBe(beforeZoom);
    // Hint overlay flashes visible. The state update propagates on the
    // next React commit, so wait for the className to flip.
    await waitFor(() => {
      expect(hint.className).toMatch(/opacity-100/);
    });
  });

  test("Cmd/Ctrl+wheel consumes the event and nudges map zoom", async () => {
    renderWithProviders(<LocationPicker value={null} onChange={() => {}} />);
    await screen.findByTestId("map-container");
    const map = getLeafletMock();
    const container = map.getContainer();
    const beforeZoom = map.getZoom();

    container.dispatchEvent(
      new WheelEvent("wheel", { deltaY: -100, ctrlKey: true, bubbles: true }),
    );
    // ctrlKey held → zoom in by one snap.
    expect(map.getZoom()).toBe(beforeZoom + 1);

    container.dispatchEvent(new WheelEvent("wheel", { deltaY: 100, metaKey: true, bubbles: true }));
    // metaKey held → zoom back out by one snap.
    expect(map.getZoom()).toBe(beforeZoom);
  });

  test("recenters the viewport when the controlled value changes", async () => {
    // `MapContainer` reads `center` / `zoom` only on initial mount, so
    // without `RecenterOnValueChange` a parent swapping `value` (e.g.
    // navigating between catalogue entries) would leave the marker
    // off-screen. Assert that the helper fires `map.setView(...)` for
    // every controlled transition: null → point, point → other point,
    // point → null.
    const { rerender } = renderWithProviders(<LocationPicker value={null} onChange={() => {}} />);
    await screen.findByTestId("map-container");
    const map = getLeafletMock();

    // First render with `value=null` still records a setView call
    // because the helper's effect always runs on mount.
    const initialCalls = map.__setViewCalls.length;

    rerender(
      <LocationPicker value={{ latitude: 37.77, longitude: -122.42 }} onChange={() => {}} />,
    );
    // null → point zooms in from the world overview to continent-level
    // framing (zoom 4) so the operator sees surrounding landmass.
    const afterFirstPoint = map.__setViewCalls.at(-1);
    expect(afterFirstPoint?.center).toEqual([37.77, -122.42]);
    expect(afterFirstPoint?.zoom).toBe(4);

    rerender(<LocationPicker value={{ latitude: 48.14, longitude: 11.58 }} onChange={() => {}} />);
    // point → point keeps the operator's current zoom (undefined ⇒ leave
    // zoom alone in real Leaflet) so a nearby re-click doesn't throw
    // them out of a close-up framing.
    const afterSecondPoint = map.__setViewCalls.at(-1);
    expect(afterSecondPoint?.center).toEqual([48.14, 11.58]);
    expect(afterSecondPoint?.zoom).toBeUndefined();

    rerender(<LocationPicker value={null} onChange={() => {}} />);
    // Clearing the value must recentre to the world overview.
    const afterClear = map.__setViewCalls.at(-1);
    expect(afterClear?.center[0]).toBe(20);
    expect(afterClear?.center[1]).toBe(0);
    expect(afterClear?.zoom).toBe(2);

    expect(map.__setViewCalls.length).toBeGreaterThan(initialCalls);
  });
});
