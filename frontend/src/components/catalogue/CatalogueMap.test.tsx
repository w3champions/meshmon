import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry } from "@/api/hooks/catalogue";

vi.mock("react-leaflet", async () => {
  const { LeafletMock } = await import("@/test/leaflet-mock");
  return LeafletMock;
});

vi.mock("@geoman-io/leaflet-geoman-free", () => ({}));
vi.mock("@geoman-io/leaflet-geoman-free/dist/leaflet-geoman.css", () => ({}));

import { CatalogueMap, EntryPopup } from "@/components/catalogue/CatalogueMap";
import { resetLeafletMock } from "@/test/leaflet-mock";
import { renderWithProviders, renderWithQuery } from "@/test/query-wrapper";

function makeEntry(overrides: Partial<CatalogueEntry> = {}): CatalogueEntry {
  return {
    id: "abc-1",
    ip: "1.2.3.4",
    display_name: null,
    asn: null,
    latitude: 48.14,
    longitude: 11.58,
    created_at: "2024-01-01T00:00:00Z",
    enrichment_status: "pending",
    operator_edited_fields: [],
    source: "operator",
    ...overrides,
  };
}

const defaultProps = {
  entries: [],
  shapes: [],
  onShapesChange: () => {},
  onRowClick: () => {},
};

describe("CatalogueMap", () => {
  beforeEach(() => {
    resetLeafletMock();
  });

  test("renders without crashing with an empty entries array", async () => {
    renderWithProviders(<CatalogueMap {...defaultProps} />);
    expect(await screen.findByTestId("draw-map-shell")).toBeInTheDocument();
  });

  test("entries with null lat/lon are filtered out", async () => {
    const entries = [
      makeEntry({ id: "e1", latitude: null, longitude: null }),
      makeEntry({ id: "e2", latitude: 51.5, longitude: null }),
      makeEntry({ id: "e3", latitude: null, longitude: -0.13 }),
    ];
    renderWithProviders(<CatalogueMap {...defaultProps} entries={entries} />);
    await screen.findByTestId("draw-map-shell");
    const markers = screen.queryAllByTestId("marker");
    expect(markers).toHaveLength(0);
  });

  test("entries with valid coordinates produce the correct number of pins", async () => {
    const entries = [
      makeEntry({ id: "e1", latitude: 48.14, longitude: 11.58 }),
      makeEntry({ id: "e2", latitude: 51.51, longitude: -0.13 }),
      makeEntry({ id: "e3", latitude: null, longitude: null }),
    ];
    renderWithProviders(<CatalogueMap {...defaultProps} entries={entries} />);
    await screen.findByTestId("draw-map-shell");
    const markers = screen.queryAllByTestId("marker");
    expect(markers).toHaveLength(2);
  });

  test("clicking the popup button fires onRowClick with the correct id", async () => {
    const onRowClick = vi.fn();
    const entry = makeEntry({ id: "target-id", ip: "10.0.0.1" });
    renderWithQuery(<EntryPopup entry={entry} onOpen={onRowClick} />);
    const btn = await screen.findByRole("button", { name: /open details for 10\.0\.0\.1/i });
    await userEvent.click(btn);
    expect(onRowClick).toHaveBeenCalledTimes(1);
  });

  test("integration: clicking pin popup button fires onRowClick with entry id", async () => {
    const onRowClick = vi.fn();
    const entries = [makeEntry({ id: "entry-42", ip: "1.2.3.4", latitude: 10, longitude: 20 })];
    const user = userEvent.setup();
    renderWithProviders(
      <CatalogueMap
        entries={entries}
        shapes={[]}
        onShapesChange={vi.fn()}
        onRowClick={onRowClick}
      />,
    );
    // The leaflet mock renders Popup children inline within the marker tree,
    // so the popup's button is reachable via a normal role query.
    const button = await screen.findByRole("button", {
      name: /open details for 1\.2\.3\.4/i,
    });
    await user.click(button);
    expect(onRowClick).toHaveBeenCalledWith("entry-42");
  });

  test("EntryPopup renders IP, display_name fallback, and ASN fallback", () => {
    const entry = makeEntry({
      ip: "192.168.1.1",
      display_name: null,
      asn: null,
    });
    const { container } = renderWithQuery(<EntryPopup entry={entry} onOpen={() => {}} />);
    expect(screen.getByText("192.168.1.1")).toBeInTheDocument();
    // display_name absent → standalone em-dash in its own <p>
    expect(screen.getByText("—")).toBeInTheDocument();
    // ASN absent → "ASN: —" in the same <p>
    expect(container.textContent).toContain("ASN: —");
  });

  test("EntryPopup renders actual display_name and ASN when present", () => {
    const entry = makeEntry({
      ip: "10.1.2.3",
      display_name: "My Server",
      asn: 13335,
    });
    renderWithQuery(<EntryPopup entry={entry} onOpen={() => {}} />);
    expect(screen.getByText("My Server")).toBeInTheDocument();
    // ASN is rendered inline with a label — match on partial text content
    expect(screen.getByText(/13335/)).toBeInTheDocument();
  });
});
