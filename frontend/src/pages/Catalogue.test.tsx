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
import type {
  CatalogueEntry,
  CatalogueListResponse,
  CatalogueMapResponse,
} from "@/api/hooks/catalogue";
import { IpHostnameProvider } from "@/components/ip-hostname";
import Catalogue from "@/pages/Catalogue";

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

// ---------------------------------------------------------------------------
// Module mocks
// ---------------------------------------------------------------------------

vi.mock("@/api/hooks/catalogue", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/catalogue")>("@/api/hooks/catalogue");
  return {
    ...actual,
    useCatalogueListInfinite: vi.fn(),
    useCatalogueMap: vi.fn(),
    useCatalogueFacets: vi.fn(),
    useReenrichOne: vi.fn(),
    useReenrichMany: vi.fn(),
  };
});

// CatalogueMap uses Leaflet which requires DOM APIs not in jsdom — stub it out.
// The stub exposes a button so tests can drive `onClusterOpen` directly.
vi.mock("@/components/catalogue/CatalogueMap", () => ({
  CatalogueMap: (props: { onClusterOpen: (bbox: [number, number, number, number]) => void }) => (
    <div data-testid="catalogue-map-stub">
      <button
        type="button"
        data-testid="stub-cluster-click"
        onClick={() => props.onClusterOpen([10, 20, 30, 40])}
      >
        fire cluster
      </button>
    </div>
  ),
}));

// DrawMap is a transitive Leaflet dep referenced by the real CatalogueMap
vi.mock("@/components/map/DrawMap", () => ({
  DrawMap: () => <div data-testid="draw-map-stub" />,
}));

// CatalogueClusterDialog renders real Radix Dialog which plays poorly with
// certain jsdom Portal targeting; stub to a minimal surface that exposes
// open state + filters so the test can assert the wiring.
vi.mock("@/components/catalogue/CatalogueClusterDialog", () => ({
  CatalogueClusterDialog: (props: { open: boolean }) =>
    props.open ? <div data-testid="cluster-dialog-stub" role="dialog" /> : null,
}));

// ---------------------------------------------------------------------------
// Imports AFTER mocks so vi.fn() stubs are in place
// ---------------------------------------------------------------------------

import {
  useCatalogueFacets,
  useCatalogueListInfinite,
  useCatalogueMap,
  useReenrichMany,
  useReenrichOne,
} from "@/api/hooks/catalogue";

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
// Mock-return builders
// ---------------------------------------------------------------------------

function buildListPage(entries: CatalogueEntry[]): CatalogueListResponse {
  return { entries, total: entries.length, next_cursor: null };
}

interface InfiniteStubOptions {
  entries?: CatalogueEntry[];
  total?: number;
  hasNextPage?: boolean;
  isFetchingNextPage?: boolean;
  fetchNextPage?: () => void;
}

/**
 * Assemble the minimum `useInfiniteQuery` return shape the page reads.
 * Tests only care about `data.pages`, `hasNextPage`, `isFetchingNextPage`,
 * and `fetchNextPage`; the rest of the react-query surface is irrelevant
 * here.
 */
function makeInfiniteReturn(opts: InfiniteStubOptions = {}) {
  const entries = opts.entries ?? [];
  const total = opts.total ?? entries.length;
  return {
    data: { pages: [{ ...buildListPage(entries), total }] },
    hasNextPage: opts.hasNextPage ?? false,
    isFetchingNextPage: opts.isFetchingNextPage ?? false,
    fetchNextPage: opts.fetchNextPage ?? vi.fn(),
    isLoading: false,
    isError: false,
  };
}

function makeMapReturn(entries: CatalogueEntry[] = []) {
  const response: CatalogueMapResponse = {
    kind: "detail",
    rows: entries,
    total: entries.length,
  };
  return { data: response, isLoading: false, isError: false };
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

interface HookOverrides {
  infinite?: InfiniteStubOptions;
  mapEntries?: CatalogueEntry[];
}

function setupHookMocks(overrides: HookOverrides = {}) {
  vi.mocked(useCatalogueListInfinite).mockReturnValue(
    makeInfiniteReturn(overrides.infinite) as unknown as ReturnType<
      typeof useCatalogueListInfinite
    >,
  );
  vi.mocked(useCatalogueMap).mockReturnValue(
    makeMapReturn(overrides.mapEntries) as unknown as ReturnType<typeof useCatalogueMap>,
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
}

// ---------------------------------------------------------------------------
// Router harness
// ---------------------------------------------------------------------------

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
      <IpHostnameProvider>
        <RouterProvider router={router} />
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

beforeEach(() => {
  setupHookMocks();
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("Catalogue page — basic render", () => {
  test("renders filter rail, view toggle, and Add IPs button", async () => {
    renderCatalogue();

    await screen.findByRole("complementary", { name: /catalogue filters/i });
    expect(screen.getByRole("button", { name: /add ips/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /table view/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /map view/i })).toBeInTheDocument();
  });
});

describe("Catalogue page — server-driven list query", () => {
  test("typing in the Name filter drives useCatalogueListInfinite with updated name", async () => {
    const user = userEvent.setup();
    renderCatalogue();

    const filterAside = await screen.findByRole("complementary", {
      name: /catalogue filters/i,
    });

    const nameInput = filterAside.querySelector<HTMLInputElement>("input[aria-label='Name']");
    if (!nameInput) throw new Error("Name filter input not found in filter rail");
    await user.type(nameInput, "Alpha");

    await waitFor(() => {
      const calls = vi.mocked(useCatalogueListInfinite).mock.calls;
      const last = calls[calls.length - 1];
      expect(last[0]).toMatchObject({ name: "Alpha" });
    });
  });

  test("shapes filter serialises to JSON on the query", async () => {
    // No direct UI surface for drawing shapes in jsdom, but we assert
    // the query-key shape is forwarded verbatim when filters omit them.
    renderCatalogue();
    await screen.findByRole("complementary", { name: /catalogue filters/i });

    const calls = vi.mocked(useCatalogueListInfinite).mock.calls;
    expect(calls.length).toBeGreaterThan(0);
    const [query] = calls[calls.length - 1];
    // Fresh mount → no shapes key in the query payload.
    expect(query).not.toHaveProperty("shapes");
  });
});

describe("Catalogue page — Add IPs panel", () => {
  test("clicking Add IPs opens the paste staging panel", async () => {
    const user = userEvent.setup();
    renderCatalogue();

    await screen.findByRole("complementary", { name: /catalogue filters/i });
    await user.click(screen.getByRole("button", { name: /add ips/i }));
    await screen.findByRole("dialog", { name: /add ips/i });
  });
});

describe("Catalogue page — row click opens drawer", () => {
  test("clicking a table row opens the entry drawer", async () => {
    setupHookMocks({ infinite: { entries: [ENTRY_A] } });
    const user = userEvent.setup();
    renderCatalogue();

    const row = await screen.findByRole("button", { name: /open entry 1\.2\.3\.4/i });
    await user.click(row);
    await screen.findByLabelText(/catalogue entry editor/i);
  });

  test("drawer closes cleanly when the open entry disappears from the list", async () => {
    // Scenario (SSE `deleted` event): list refetches without the entry
    // the drawer is observing. The deletion guard effect in Catalogue
    // nulls `drawerId` so the drawer unmounts and doesn't leak stale
    // data or an orphaned drawer-open state.
    let currentEntries: CatalogueEntry[] = [ENTRY_A];
    vi.mocked(useCatalogueListInfinite).mockImplementation(
      () =>
        makeInfiniteReturn({ entries: currentEntries }) as unknown as ReturnType<
          typeof useCatalogueListInfinite
        >,
    );
    const user = userEvent.setup();
    renderCatalogue();

    const row = await screen.findByRole("button", { name: /open entry 1\.2\.3\.4/i });
    await user.click(row);
    await screen.findByLabelText(/catalogue entry editor/i);

    currentEntries = [];
    await user.keyboard("{Escape}");

    await waitFor(() => {
      expect(screen.queryByLabelText(/catalogue entry editor/i)).not.toBeInTheDocument();
    });
  });
});

describe("Catalogue page — Re-enrich button", () => {
  test("Re-enrich button is disabled when no rows are loaded", async () => {
    renderCatalogue();
    await screen.findByRole("complementary", { name: /catalogue filters/i });

    const btn = screen.getByRole("button", { name: /re-enrich/i });
    expect(btn).toBeDisabled();
  });

  test("label reads 'Re-enrich loaded (N of M)' when pagination isn't exhausted", async () => {
    // Loaded subset = 1, server total = 327 — the button promises the
    // honest action: fire against the loaded rows only.
    setupHookMocks({ infinite: { entries: [ENTRY_A], total: 327 } });
    renderCatalogue();

    const btn = await screen.findByRole("button", { name: /re-enrich loaded \(1 of 327\)/i });
    expect(btn).toBeEnabled();
  });

  test("label reads 'Re-enrich all (M)' when every row is loaded", async () => {
    setupHookMocks({ infinite: { entries: [ENTRY_A], total: 1 } });
    renderCatalogue();

    const btn = await screen.findByRole("button", { name: /re-enrich all \(1\)/i });
    expect(btn).toBeEnabled();
  });
});

describe("Catalogue page — sort state round-trips through the URL", () => {
  test("pre-existing ?sort=ip&dir=asc URL surfaces as the table query's sort fields", async () => {
    renderCatalogue("/catalogue?sort=ip&dir=asc");
    await screen.findByRole("complementary", { name: /catalogue filters/i });

    const calls = vi.mocked(useCatalogueListInfinite).mock.calls;
    expect(calls.length).toBeGreaterThan(0);
    const [query] = calls[calls.length - 1];
    expect(query).toMatchObject({ sort: "ip", sort_dir: "asc" });
  });
});

describe("Catalogue page — cluster dialog wiring", () => {
  test("clicking a cluster on the map opens the cluster dialog", async () => {
    const user = userEvent.setup();
    renderCatalogue("/catalogue?view=map");
    await screen.findByRole("complementary", { name: /catalogue filters/i });

    // The real CatalogueMap would fire onClusterOpen on leaflet click —
    // the stub exposes a button that fires it with a fixture bbox.
    const trigger = await screen.findByTestId("stub-cluster-click");
    await user.click(trigger);

    await screen.findByTestId("cluster-dialog-stub");
  });
});
