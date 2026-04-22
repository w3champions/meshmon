import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, test, vi } from "vitest";
import type {
  CatalogueEntry,
  CatalogueMapBucket,
  CatalogueMapResponse,
} from "@/api/hooks/catalogue";

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
import { fireClusterClick, getLeafletMock, resetLeafletMock } from "@/test/leaflet-mock";
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

function detailResponse(rows: CatalogueEntry[]): CatalogueMapResponse {
  return { kind: "detail", rows, total: rows.length };
}

function clusterBucket(overrides: Partial<CatalogueMapBucket> = {}): CatalogueMapBucket {
  return {
    bbox: [-1, -1, 1, 1],
    count: 10,
    lat: 0,
    lng: 0,
    sample_id: "00000000-0000-0000-0000-000000000000",
    ...overrides,
  };
}

function clustersResponse(buckets: CatalogueMapBucket[]): CatalogueMapResponse {
  return { kind: "clusters", buckets, cell_size: 1, total: 999 };
}

const noopHandlers = {
  onShapesChange: () => {},
  onOpenEntry: () => {},
  onClusterOpen: () => {},
  onViewportChange: () => {},
};

describe("CatalogueMap", () => {
  beforeEach(() => {
    resetLeafletMock();
  });

  test("renders the DrawMap shell without a response", async () => {
    renderWithProviders(
      <CatalogueMap
        response={undefined}
        isLoading={true}
        isError={false}
        shapes={[]}
        {...noopHandlers}
      />,
    );
    expect(await screen.findByTestId("draw-map-shell")).toBeInTheDocument();
    // No pins render while loading.
    expect(screen.queryAllByTestId("marker")).toHaveLength(0);
  });

  test("detail-kind response renders one pin per row with lat/lon", async () => {
    const entries = [
      makeEntry({ id: "e1", latitude: 48.14, longitude: 11.58 }),
      makeEntry({ id: "e2", latitude: 51.51, longitude: -0.13 }),
      makeEntry({ id: "e3", latitude: null, longitude: null }),
    ];
    renderWithProviders(
      <CatalogueMap
        response={detailResponse(entries)}
        isLoading={false}
        isError={false}
        shapes={[]}
        {...noopHandlers}
      />,
    );
    await screen.findByTestId("draw-map-shell");
    const markers = screen.queryAllByTestId("marker");
    expect(markers).toHaveLength(2);
  });

  test("clusters-kind response renders one marker per bucket with count label", async () => {
    const buckets = [
      clusterBucket({ count: 5, lat: 10, lng: 20, sample_id: "s1" }),
      clusterBucket({ count: 13, lat: 11, lng: 21, sample_id: "s2" }),
      clusterBucket({ count: 42, lat: 12, lng: 22, sample_id: "s3" }),
    ];
    renderWithProviders(
      <CatalogueMap
        response={clustersResponse(buckets)}
        isLoading={false}
        isError={false}
        shapes={[]}
        {...noopHandlers}
      />,
    );
    await screen.findByTestId("draw-map-shell");
    const markers = screen.queryAllByTestId("marker");
    expect(markers).toHaveLength(3);
    // Cluster markers carry a DivIcon; detail pins don't.
    for (const marker of markers) {
      expect(marker).toHaveAttribute("data-has-icon", "true");
    }
  });

  test("clicking a cluster marker fires onClusterOpen with its bbox", async () => {
    const onClusterOpen = vi.fn();
    const bucket = clusterBucket({
      count: 5,
      lat: 10,
      lng: 20,
      bbox: [9, 19, 11, 21],
      sample_id: "s1",
    });
    const user = userEvent.setup();
    renderWithProviders(
      <CatalogueMap
        response={clustersResponse([bucket])}
        isLoading={false}
        isError={false}
        shapes={[]}
        onShapesChange={() => {}}
        onOpenEntry={() => {}}
        onClusterOpen={onClusterOpen}
        onViewportChange={() => {}}
      />,
    );
    await screen.findByTestId("draw-map-shell");
    const marker = screen.getByTestId("marker");
    await user.click(marker);
    expect(onClusterOpen).toHaveBeenCalledWith([9, 19, 11, 21]);
  });

  test("clusters-mode bypasses MarkerClusterGroup (server already aggregated)", async () => {
    const buckets = [clusterBucket({ count: 5 })];
    renderWithProviders(
      <CatalogueMap
        response={clustersResponse(buckets)}
        isLoading={false}
        isError={false}
        shapes={[]}
        {...noopHandlers}
      />,
    );
    await screen.findByTestId("draw-map-shell");
    // `clusterMode` short-circuits `react-leaflet-cluster` — the wrapper
    // never mounts.
    expect(screen.queryByTestId("marker-cluster-group")).not.toBeInTheDocument();
  });

  test("detail-mode renders through the MarkerClusterGroup wrapper", async () => {
    const entries = [makeEntry({ id: "e1", latitude: 1, longitude: 2 })];
    renderWithProviders(
      <CatalogueMap
        response={detailResponse(entries)}
        isLoading={false}
        isError={false}
        shapes={[]}
        {...noopHandlers}
      />,
    );
    await screen.findByTestId("draw-map-shell");
    expect(screen.getByTestId("marker-cluster-group")).toBeInTheDocument();
  });

  test("detail-mode cluster click fires onClusterOpen with a bbox enclosing the pins", async () => {
    const onClusterOpen = vi.fn();
    const entries = [
      makeEntry({ id: "a", latitude: 10, longitude: 20 }),
      makeEntry({ id: "b", latitude: 12, longitude: 24 }),
    ];
    renderWithProviders(
      <CatalogueMap
        response={detailResponse(entries)}
        isLoading={false}
        isError={false}
        shapes={[]}
        onShapesChange={() => {}}
        onOpenEntry={() => {}}
        onClusterOpen={onClusterOpen}
        onViewportChange={() => {}}
      />,
    );
    await screen.findByTestId("draw-map-shell");
    // `MarkerClusterGroup` only receives an `onClick` when
    // `onClusterClick` is wired — assert that first so a regression
    // where the wiring is lost surfaces here.
    const clusterGroup = screen.getByTestId("marker-cluster-group");
    expect(clusterGroup).toHaveAttribute("data-has-on-click", "true");

    // Simulate a cluster click carrying both pin ids.
    fireClusterClick(["a", "b"]);

    expect(onClusterOpen).toHaveBeenCalledTimes(1);
    const [bbox] = onClusterOpen.mock.calls[0];
    const [minLat, minLng, maxLat, maxLng] = bbox as [number, number, number, number];
    // Bbox must enclose every clustered pin — with a small epsilon pad
    // so backend BETWEEN filters don't drop edge pins.
    expect(minLat).toBeLessThanOrEqual(10);
    expect(maxLat).toBeGreaterThanOrEqual(12);
    expect(minLng).toBeLessThanOrEqual(20);
    expect(maxLng).toBeGreaterThanOrEqual(24);
  });

  test("cluster-mode does not wire onClusterClick (server already aggregated)", async () => {
    const buckets = [clusterBucket({ count: 5 })];
    renderWithProviders(
      <CatalogueMap
        response={clustersResponse(buckets)}
        isLoading={false}
        isError={false}
        shapes={[]}
        {...noopHandlers}
      />,
    );
    await screen.findByTestId("draw-map-shell");
    // In cluster mode the wrapper is bypassed entirely — no MarkerClusterGroup,
    // no onClusterClick to worry about. Guard this invariant.
    expect(screen.queryByTestId("marker-cluster-group")).not.toBeInTheDocument();
  });

  test("isError shows an error overlay AND keeps the map interactive", async () => {
    renderWithProviders(
      <CatalogueMap
        response={undefined}
        isLoading={false}
        isError={true}
        shapes={[]}
        {...noopHandlers}
      />,
    );
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/failed to load/i);
    // The map must stay mounted in error state: the advertised recover
    // path ("Pan or zoom to retry") needs `moveend` to fire, and that
    // only happens while `DrawMap` is in the tree.
    expect(screen.getByTestId("draw-map-shell")).toBeInTheDocument();
  });

  test("viewport change emits bbox and zoom on moveend", async () => {
    const onViewportChange = vi.fn();
    renderWithProviders(
      <CatalogueMap
        response={undefined}
        isLoading={true}
        isError={false}
        shapes={[]}
        onShapesChange={() => {}}
        onOpenEntry={() => {}}
        onClusterOpen={() => {}}
        onViewportChange={onViewportChange}
      />,
    );
    await screen.findByTestId("draw-map-shell");

    // ViewportController emits the initial viewport on mount. Seed
    // `__bounds` and fire `moveend` to verify the published payload.
    const map = getLeafletMock();
    map.__bounds = [10, 20, 30, 40];
    map.__zoom = 5;
    map.__fire("moveend");

    expect(onViewportChange).toHaveBeenCalled();
    const [bbox, zoom] = onViewportChange.mock.calls.at(-1) ?? [];
    expect(bbox).toEqual([10, 20, 30, 40]);
    expect(zoom).toBe(5);
  });

  test("viewport clamps longitudes wrapped past the antimeridian", async () => {
    const onViewportChange = vi.fn();
    renderWithProviders(
      <CatalogueMap
        response={undefined}
        isLoading={true}
        isError={false}
        shapes={[]}
        onShapesChange={() => {}}
        onOpenEntry={() => {}}
        onClusterOpen={() => {}}
        onViewportChange={onViewportChange}
      />,
    );
    await screen.findByTestId("draw-map-shell");

    const map = getLeafletMock();
    // Simulate Leaflet `worldCopyJump` bounds after the operator pans
    // east past +180°: sw.lng = 170, ne.lng = 220. The backend rejects
    // lng > 180 with 400, so `DrawMap` must clamp before emitting.
    map.__bounds = [-10, 170, 10, 220];
    map.__zoom = 3;
    map.__fire("moveend");

    const [bbox] = onViewportChange.mock.calls.at(-1) ?? [];
    const [, , , maxLng] = bbox as [number, number, number, number];
    expect(maxLng).toBe(180);
  });

  test("clicking EntryPopup 'Open details' button fires the supplied callback", async () => {
    const onRowClick = vi.fn();
    const entry = makeEntry({ id: "target-id", ip: "10.0.0.1" });
    renderWithQuery(<EntryPopup entry={entry} onOpen={onRowClick} />);
    const btn = await screen.findByRole("button", { name: /open details for 10\.0\.0\.1/i });
    await userEvent.click(btn);
    expect(onRowClick).toHaveBeenCalledTimes(1);
  });

  test("integration: detail popup button fires onOpenEntry with the full entry", async () => {
    const onOpenEntry = vi.fn();
    const entries = [makeEntry({ id: "entry-42", ip: "1.2.3.4", latitude: 10, longitude: 20 })];
    const user = userEvent.setup();
    renderWithProviders(
      <CatalogueMap
        response={detailResponse(entries)}
        isLoading={false}
        isError={false}
        shapes={[]}
        onShapesChange={vi.fn()}
        onOpenEntry={onOpenEntry}
        onClusterOpen={() => {}}
        onViewportChange={() => {}}
      />,
    );
    const button = await screen.findByRole("button", {
      name: /open details for 1\.2\.3\.4/i,
    });
    await user.click(button);
    // Full entry must be passed so the parent can seed the drawer when
    // `rows.find(id)` would miss (map omits `city`/`shapes` filters).
    expect(onOpenEntry).toHaveBeenCalledWith(
      expect.objectContaining({ id: "entry-42", ip: "1.2.3.4" }),
    );
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
    // IP renders in the header AND in the Hostname meta row (the row always
    // appears; the shared `<IpHostname>` falls back to the bare IP until the
    // provider has a positive hit).
    expect(screen.getAllByText("192.168.1.1").length).toBeGreaterThanOrEqual(1);
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
    // IP appears twice — once under the display_name in the header and once
    // in the Hostname meta row; `getAllByText` keeps the assertion honest.
    expect(screen.getAllByText("10.1.2.3").length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText("Hong Kong, Hong Kong")).toBeInTheDocument();
    expect(screen.getByText("AS13335 · Cloudflare")).toBeInTheDocument();
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
});
