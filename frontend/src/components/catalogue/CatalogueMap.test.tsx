import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry } from "@/api/hooks/catalogue";

vi.mock("react-leaflet", async () => {
  const { LeafletMock } = await import("@/test/leaflet-mock");
  return LeafletMock;
});

vi.mock("@geoman-io/leaflet-geoman-free", () => ({}));
vi.mock("@geoman-io/leaflet-geoman-free/dist/leaflet-geoman.css", () => ({}));

vi.mock("leaflet.markercluster/dist/MarkerCluster.css", () => ({}));
vi.mock("leaflet.markercluster/dist/MarkerCluster.Default.css", () => ({}));
vi.mock("react-leaflet-cluster", async () => {
  const { MarkerClusterGroupMock } = await import("@/test/leaflet-mock");
  return { default: MarkerClusterGroupMock };
});

import { CatalogueMap, EntryPopup } from "@/components/catalogue/CatalogueMap";
import { fireClusterClick, resetLeafletMock } from "@/test/leaflet-mock";
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

  test("EntryPopup shows IP as header when display_name absent and hides optional rows", () => {
    const entry = makeEntry({
      ip: "192.168.1.1",
      display_name: null,
      asn: null,
      city: null,
      country_name: null,
      country_code: null,
      network_operator: null,
      website: null,
      notes: null,
    });
    renderWithQuery(<EntryPopup entry={entry} onOpen={() => {}} />);
    // Header falls back to the IP address when display_name is absent.
    expect(screen.getByText("192.168.1.1")).toBeInTheDocument();
    // Optional rows are hidden entirely when their source field is absent.
    // No location/network "—" placeholder rows should appear.
    expect(screen.queryByText(/AS\s*—/i)).not.toBeInTheDocument();
  });

  test("EntryPopup renders display_name, IP, location, network, and website", () => {
    const entry = makeEntry({
      ip: "10.1.2.3",
      display_name: "My Server",
      asn: 13335,
      city: "Hong Kong",
      country_name: "Hong Kong",
      network_operator: "Cloudflare",
      website: "https://cloudflare.com/about",
    });
    renderWithQuery(<EntryPopup entry={entry} onOpen={() => {}} />);
    expect(screen.getByText("My Server")).toBeInTheDocument();
    expect(screen.getByText("10.1.2.3")).toBeInTheDocument();
    expect(screen.getByText("Hong Kong, Hong Kong")).toBeInTheDocument();
    expect(screen.getByText("AS13335 · Cloudflare")).toBeInTheDocument();
    // Website is rendered as hostname-only link with target=_blank.
    const link = screen.getByRole("link", { name: "cloudflare.com" });
    expect(link).toHaveAttribute("href", "https://cloudflare.com/about");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
  });

  test("EntryPopup falls back to lookupCountryName when country_name missing", () => {
    const entry = makeEntry({
      city: "Frankfurt",
      country_name: null,
      country_code: "DE",
    });
    renderWithQuery(<EntryPopup entry={entry} onOpen={() => {}} />);
    expect(screen.getByText("Frankfurt, Germany")).toBeInTheDocument();
  });

  test("EntryPopup truncates notes to the first line", () => {
    const entry = makeEntry({
      notes: "first line of the note\nsecond line that must be hidden",
    });
    renderWithQuery(<EntryPopup entry={entry} onOpen={() => {}} />);
    expect(screen.getByText("first line of the note")).toBeInTheDocument();
    expect(screen.queryByText("second line that must be hidden")).not.toBeInTheDocument();
  });

  test("cluster click opens the cluster dialog listing the cluster's members", async () => {
    const entries = [
      makeEntry({ id: "e1", ip: "1.1.1.1", display_name: "Alpha", latitude: 10, longitude: 20 }),
      makeEntry({ id: "e2", ip: "2.2.2.2", display_name: "Beta", latitude: 11, longitude: 21 }),
      makeEntry({ id: "e3", ip: "3.3.3.3", display_name: "Gamma", latitude: 12, longitude: 22 }),
    ];
    renderWithProviders(
      <CatalogueMap
        entries={entries}
        shapes={[]}
        onShapesChange={() => {}}
        onRowClick={() => {}}
      />,
    );
    await screen.findByTestId("draw-map-shell");
    // Before the click the dialog should not be rendered.
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    // Simulate the cluster click: the user clicked a cluster that contains
    // e1 and e3 (but not e2).
    fireClusterClick(["e1", "e3"]);

    const dialog = await screen.findByRole("dialog");
    const dialogUtils = within(dialog);
    expect(dialogUtils.getByText("2 pins in this area")).toBeInTheDocument();
    expect(dialogUtils.getByText("Alpha")).toBeInTheDocument();
    expect(dialogUtils.getByText("Gamma")).toBeInTheDocument();
    expect(dialogUtils.queryByText("Beta")).not.toBeInTheDocument();
  });

  test("clicking a row in the cluster dialog fires onRowClick with that entry's id", async () => {
    const onRowClick = vi.fn();
    const entries = [
      makeEntry({ id: "e1", ip: "1.1.1.1", display_name: "Alpha", latitude: 10, longitude: 20 }),
      makeEntry({ id: "e2", ip: "2.2.2.2", display_name: "Beta", latitude: 11, longitude: 21 }),
    ];
    const user = userEvent.setup();
    renderWithProviders(
      <CatalogueMap
        entries={entries}
        shapes={[]}
        onShapesChange={() => {}}
        onRowClick={onRowClick}
      />,
    );
    await screen.findByTestId("draw-map-shell");
    fireClusterClick(["e1", "e2"]);

    const dialog = await screen.findByRole("dialog");
    const betaButton = within(dialog).getByRole("button", {
      name: /open details for Beta/i,
    });
    await user.click(betaButton);
    expect(onRowClick).toHaveBeenCalledWith("e2");
  });
});
