import { screen } from "@testing-library/react";
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
      <LocationPicker
        value={{ latitude: 48.14, longitude: 11.58 }}
        onChange={() => {}}
      />,
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
      <LocationPicker
        value={{ latitude: 1, longitude: 2 }}
        onChange={handler}
      />,
    );
    await screen.findByTestId("marker");

    fireMarkerDragEnd(10.5, 20.25);

    expect(handler).toHaveBeenCalledWith({ latitude: 10.5, longitude: 20.25 });
  });

  test("clicking Clear fires onChange(null) and is keyboard-operable", async () => {
    const handler = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(
      <LocationPicker
        value={{ latitude: 1, longitude: 2 }}
        onChange={handler}
      />,
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
});
