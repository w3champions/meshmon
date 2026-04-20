import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import Catalogue from "@/pages/Catalogue";

// ---------------------------------------------------------------------------
// Module mocks
// ---------------------------------------------------------------------------

vi.mock("@/api/hooks/catalogue", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/catalogue")>("@/api/hooks/catalogue");
  return {
    ...actual,
    useCatalogueList: vi.fn(),
    useCatalogueFacets: vi.fn(),
    useReenrichOne: vi.fn(),
    useReenrichMany: vi.fn(),
  };
});

vi.mock("@/api/hooks/catalogue-stream", () => ({
  useCatalogueStream: vi.fn(),
}));

// CatalogueMap uses Leaflet which requires DOM APIs not in jsdom — stub it out.
vi.mock("@/components/catalogue/CatalogueMap", () => ({
  CatalogueMap: () => <div data-testid="catalogue-map-stub" />,
}));

// DrawMap is a transitive Leaflet dep referenced by the real CatalogueMap
vi.mock("@/components/map/DrawMap", () => ({
  DrawMap: () => <div data-testid="draw-map-stub" />,
}));

// ---------------------------------------------------------------------------
// Imports AFTER mocks so vi.fn() stubs are in place
// ---------------------------------------------------------------------------

import {
  useCatalogueFacets,
  useCatalogueList,
  useReenrichMany,
  useReenrichOne,
} from "@/api/hooks/catalogue";
import { useCatalogueStream } from "@/api/hooks/catalogue-stream";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const ENTRY_A: CatalogueEntry = {
  id: "entry-a",
  ip: "1.2.3.4",
  display_name: "Alpha Node",
  city: "Amsterdam",
  country_code: "NL",
  country_name: "Netherlands",
  asn: 1234,
  network_operator: "Example ISP",
  enrichment_status: "enriched",
  operator_edited_fields: [],
  created_at: "2026-01-01T00:00:00Z",
  source: "operator",
  latitude: 52.37,
  longitude: 4.9,
};

// ---------------------------------------------------------------------------
// Render helper
// ---------------------------------------------------------------------------

function makeMockReturn(entries: CatalogueEntry[]) {
  return {
    data: { entries, total: entries.length },
    isLoading: false,
    isError: false,
  };
}

const FACETS_STUB = {
  data: {
    countries: [{ code: "NL", name: "Netherlands", count: 1 }],
    asns: [] as { asn: number; count: number }[],
    networks: [] as { name: string; count: number }[],
    cities: [] as { name: string; count: number }[],
  },
  isLoading: false,
  isError: false,
};

const REENRICH_ONE_STUB = { mutate: vi.fn(), isPending: false };
const REENRICH_MANY_STUB = { mutate: vi.fn(), isPending: false };

function setupHookMocks(entries: CatalogueEntry[] = []) {
  vi.mocked(useCatalogueList).mockReturnValue(
    makeMockReturn(entries) as ReturnType<typeof useCatalogueList>,
  );
  vi.mocked(useCatalogueFacets).mockReturnValue(
    FACETS_STUB as unknown as ReturnType<typeof useCatalogueFacets>,
  );
  vi.mocked(useReenrichOne).mockReturnValue(
    REENRICH_ONE_STUB as unknown as ReturnType<typeof useReenrichOne>,
  );
  vi.mocked(useReenrichMany).mockReturnValue(
    REENRICH_MANY_STUB as unknown as ReturnType<typeof useReenrichMany>,
  );
  vi.mocked(useCatalogueStream).mockReturnValue(undefined);
}

function renderCatalogue(initialPath = "/catalogue") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });

  const rootRoute = createRootRoute({ component: Outlet });
  const catalogueRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/catalogue",
    component: Catalogue,
  });

  const router = createRouter({
    routeTree: rootRoute.addChildren([catalogueRoute]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
  });

  return render(
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

beforeEach(() => {
  setupHookMocks();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("Catalogue page — basic render", () => {
  test("renders filter rail, view toggle, and Add IPs button", async () => {
    renderCatalogue();

    // Filter rail landmark
    await screen.findByRole("complementary", { name: /catalogue filters/i });

    // Add IPs button
    expect(screen.getByRole("button", { name: /add ips/i })).toBeInTheDocument();

    // View toggle buttons
    expect(screen.getByRole("button", { name: /table view/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /map view/i })).toBeInTheDocument();
  });

  test("calls useCatalogueStream once on mount", async () => {
    renderCatalogue();
    await screen.findByRole("complementary", { name: /catalogue filters/i });
    expect(useCatalogueStream).toHaveBeenCalledTimes(1);
  });
});

describe("Catalogue page — filter interaction", () => {
  test("typing in the Name filter calls useCatalogueList with updated name param", async () => {
    const user = userEvent.setup();
    renderCatalogue();

    // Wait for render
    const filterAside = await screen.findByRole("complementary", {
      name: /catalogue filters/i,
    });

    // The Name input is always rendered (FreeTextGroup is an open/close details).
    // The aria-label on the input matches the title prop ("Name").
    // There are multiple "Name" texts (filter label + table header), so query
    // specifically within the filter aside.
    const nameInput = filterAside.querySelector<HTMLInputElement>("input[aria-label='Name']");
    if (!nameInput) throw new Error("Name filter input not found in filter rail");

    await user.type(nameInput, "Alpha");

    await waitFor(() => {
      const calls = vi.mocked(useCatalogueList).mock.calls;
      const lastCall = calls[calls.length - 1];
      expect(lastCall[0]).toMatchObject({ name: "Alpha" });
    });
  });
});

describe("Catalogue page — Add IPs panel", () => {
  test("clicking Add IPs opens the paste staging panel", async () => {
    const user = userEvent.setup();
    renderCatalogue();

    await screen.findByRole("complementary", { name: /catalogue filters/i });

    const addIpsButton = screen.getByRole("button", { name: /add ips/i });
    await user.click(addIpsButton);

    await screen.findByRole("region", { name: /paste ips staging panel/i });
  });
});

describe("Catalogue page — row click opens drawer", () => {
  test("clicking a table row opens the entry drawer", async () => {
    vi.mocked(useCatalogueList).mockReturnValue(
      makeMockReturn([ENTRY_A]) as ReturnType<typeof useCatalogueList>,
    );
    const user = userEvent.setup();
    renderCatalogue();

    // Wait for the row to appear
    const row = await screen.findByRole("button", { name: /open entry 1\.2\.3\.4/i });
    await user.click(row);

    // Entry drawer should appear — identified by its aria-label
    await screen.findByLabelText(/catalogue entry editor/i);
  });
});

describe("Catalogue page — Re-enrich all button", () => {
  test("Re-enrich all button is visible but disabled when no entries", async () => {
    renderCatalogue();
    await screen.findByRole("complementary", { name: /catalogue filters/i });

    const btn = screen.getByRole("button", { name: /re-enrich all/i });
    expect(btn).toBeDisabled();
  });

  test("Re-enrich all button is enabled when entries are present", async () => {
    vi.mocked(useCatalogueList).mockReturnValue(
      makeMockReturn([ENTRY_A]) as ReturnType<typeof useCatalogueList>,
    );
    renderCatalogue();

    const btn = await screen.findByRole("button", { name: /re-enrich all/i });
    expect(btn).not.toBeDisabled();
  });
});
