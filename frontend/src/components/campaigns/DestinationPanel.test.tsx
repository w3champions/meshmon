import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { type ReactElement, type ReactNode, useEffect } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry, CatalogueListResponse } from "@/api/hooks/catalogue";
import * as catalogueHook from "@/api/hooks/catalogue";
import { DestinationPanel } from "@/components/campaigns/DestinationPanel";
import type { FilterValue } from "@/components/filter/FilterRail";
import { useIpHostnameContext } from "@/components/ip-hostname/IpHostnameProvider";
import { renderWithQuery } from "@/test/query-wrapper";

class MockEventSource {
  static instances: MockEventSource[] = [];
  listeners: Record<string, Array<(event: { data: string }) => void>> = {};
  constructor(public url: string) {
    MockEventSource.instances.push(this);
  }
  addEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name] ?? [];
    list.push(handler);
    this.listeners[name] = list;
  }
  removeEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name];
    if (!list) return;
    const idx = list.indexOf(handler);
    if (idx >= 0) list.splice(idx, 1);
  }
  close(): void {}
}

interface SeedEntry {
  ip: string;
  hostname?: string | null;
}

function Seeder({ seed, children }: { seed: SeedEntry[]; children: ReactNode }) {
  const { seedFromResponse } = useIpHostnameContext();
  // biome-ignore lint/correctness/useExhaustiveDependencies: mount-only seed
  useEffect(() => {
    if (seed.length > 0) seedFromResponse(seed);
  }, []);
  return <>{children}</>;
}

function renderPanel(ui: ReactElement, seed: SeedEntry[] = []) {
  return renderWithQuery(<Seeder seed={seed}>{ui}</Seeder>);
}

vi.mock("@/api/hooks/catalogue");

// The PasteStaging dialog is heavy and pulls Leaflet; replace it with a
// deterministic stand-in that exposes a "fire success" button we can
// click from tests.
vi.mock("@/components/catalogue/PasteStaging", () => ({
  PasteStaging: ({
    open,
    onPasteSuccess,
  }: {
    open: boolean;
    onPasteSuccess?: (ips: string[]) => void;
  }) => {
    if (!open) return null;
    return (
      <div data-testid="paste-staging-mock">
        <button type="button" onClick={() => onPasteSuccess?.(["1.2.3.4"])}>
          Simulate paste success
        </button>
      </div>
    );
  },
}));

const pushToast = vi.fn();
vi.mock("@/stores/toast", () => ({
  useToastStore: {
    getState: () => ({ pushToast }),
  },
}));

const ENTRY_A: CatalogueEntry = {
  id: "a",
  ip: "10.0.0.1",
  display_name: "Alpha",
  city: "Amsterdam",
  country_code: "NL",
  country_name: "Netherlands",
  asn: 1,
  network_operator: "AlphaNet",
  enrichment_status: "enriched",
  operator_edited_fields: [],
  created_at: "2026-01-01T00:00:00Z",
  source: "operator",
};

const ENTRY_B: CatalogueEntry = {
  id: "b",
  ip: "10.0.0.2",
  display_name: "Beta",
  city: "Berlin",
  country_code: "DE",
  country_name: "Germany",
  asn: 2,
  network_operator: "BetaNet",
  enrichment_status: "enriched",
  operator_edited_fields: [],
  created_at: "2026-01-02T00:00:00Z",
  source: "operator",
};

const ENTRY_C: CatalogueEntry = {
  id: "c",
  ip: "10.0.0.3",
  display_name: "Gamma",
  city: "Berlin",
  country_code: "DE",
  country_name: "Germany",
  asn: 2,
  network_operator: "BetaNet",
  enrichment_status: "enriched",
  operator_edited_fields: [],
  created_at: "2026-01-03T00:00:00Z",
  source: "operator",
};

const EMPTY_FILTER: FilterValue = {
  countryCodes: [],
  asns: [],
  networks: [],
  cities: [],
  shapes: [],
};

function pagesOf(pages: CatalogueEntry[][], total = -1): CatalogueListResponse[] {
  return pages.map((entries, idx) => ({
    entries,
    total: total === -1 ? pages.flat().length : total,
    next_cursor: idx + 1 < pages.length ? `cursor-${idx}` : null,
  }));
}

interface MockListOptions {
  pages: CatalogueListResponse[];
  hasNextPage?: boolean;
  fetchNextPage?: ReturnType<typeof vi.fn>;
  isLoading?: boolean;
  isFetching?: boolean;
}

function mockList(opts: MockListOptions) {
  const hasNext = opts.hasNextPage ?? Boolean(opts.pages.at(-1)?.next_cursor);
  vi.mocked(catalogueHook.useCatalogueListInfinite).mockReturnValue({
    data: {
      pages: opts.pages,
      pageParams: opts.pages.map((_, i) => (i === 0 ? undefined : `cursor-${i - 1}`)),
    },
    isLoading: opts.isLoading ?? false,
    isFetching: opts.isFetching ?? false,
    isError: false,
    hasNextPage: hasNext,
    isFetchingNextPage: false,
    fetchNextPage: opts.fetchNextPage ?? vi.fn(),
  } as unknown as ReturnType<typeof catalogueHook.useCatalogueListInfinite>);
}

beforeEach(() => {
  mockList({ pages: pagesOf([[ENTRY_A, ENTRY_B]]) });
  pushToast.mockReset();
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("DestinationPanel", () => {
  test("'Add all' walks remaining pages and merges every IP into selected", async () => {
    // One page loaded initially, two more land via fetchNextPage. The
    // panel must walk the cursor chain to completion and emit a Set
    // containing every IP across all pages.
    const page1 = pagesOf([[ENTRY_A]], 3)[0];
    const page2 = pagesOf([[ENTRY_B]], 3)[0];
    const page3 = pagesOf([[ENTRY_C]], 3)[0];
    // Cursor wiring: page 1 has a next cursor (since pages 2/3 follow);
    // page 2 still has a cursor for page 3; page 3 terminates.
    page1.next_cursor = "cursor-1";
    page2.next_cursor = "cursor-2";
    page3.next_cursor = null;

    const fetchNextPage = vi
      .fn()
      .mockResolvedValueOnce({
        data: { pages: [page1, page2], pageParams: [undefined, "cursor-1"] },
        hasNextPage: true,
      })
      .mockResolvedValueOnce({
        data: { pages: [page1, page2, page3], pageParams: [undefined, "cursor-1", "cursor-2"] },
        hasNextPage: false,
      });

    mockList({ pages: [page1], hasNextPage: true, fetchNextPage });

    const onSelectedChange = vi.fn<(next: Set<string>) => void>();
    const user = userEvent.setup();

    renderWithQuery(
      <DestinationPanel
        selected={new Set()}
        onSelectedChange={onSelectedChange}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: /^add all$/i }));

    await waitFor(() => {
      expect(fetchNextPage).toHaveBeenCalledTimes(2);
    });

    await waitFor(() => {
      expect(onSelectedChange).toHaveBeenCalled();
    });

    const snapshot = onSelectedChange.mock.calls.at(-1)?.[0];
    expect(Array.from(snapshot ?? []).sort()).toEqual(["10.0.0.1", "10.0.0.2", "10.0.0.3"]);
  });

  test("'Add all' merges into existing selection instead of replacing it", async () => {
    // Prior manual pick: 192.168.1.1 via row click. The walk finds A +
    // B; the emitted snapshot must carry all three, not just the walk
    // output.
    mockList({ pages: pagesOf([[ENTRY_A, ENTRY_B]]) });

    const onSelectedChange = vi.fn<(next: Set<string>) => void>();
    const user = userEvent.setup();

    renderWithQuery(
      <DestinationPanel
        selected={new Set(["192.168.1.1"])}
        onSelectedChange={onSelectedChange}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: /^add all$/i }));

    await waitFor(() => {
      expect(onSelectedChange).toHaveBeenCalled();
    });
    const snapshot = onSelectedChange.mock.calls.at(-1)?.[0];
    expect(Array.from(snapshot ?? []).sort()).toEqual(["10.0.0.1", "10.0.0.2", "192.168.1.1"]);
  });

  test("button is disabled while the catalogue is still loading", async () => {
    mockList({ pages: [], isLoading: true, isFetching: true });

    const onSelectedChange = vi.fn<(next: Set<string>) => void>();
    const user = userEvent.setup();

    renderWithQuery(
      <DestinationPanel
        selected={new Set()}
        onSelectedChange={onSelectedChange}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    const btn = screen.getByRole("button", { name: /^add all$/i });
    expect(btn).toBeDisabled();
    await user.click(btn);

    expect(onSelectedChange).not.toHaveBeenCalled();
    expect(pushToast).not.toHaveBeenCalled();
  });

  test("fetchNextPage rejection surfaces walk error + toast, selection untouched", async () => {
    const page1 = pagesOf([[ENTRY_A]], 10)[0];
    page1.next_cursor = "cursor-1";
    const fetchNextPage = vi.fn().mockRejectedValueOnce(new Error("boom"));

    mockList({ pages: [page1], hasNextPage: true, fetchNextPage });

    const onSelectedChange = vi.fn<(next: Set<string>) => void>();
    const user = userEvent.setup();

    renderWithQuery(
      <DestinationPanel
        selected={new Set()}
        onSelectedChange={onSelectedChange}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: /^add all$/i }));

    await waitFor(() => {
      expect(pushToast).toHaveBeenCalledWith(
        expect.objectContaining({ kind: "error", message: "boom" }),
      );
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(/walk failed: boom/i);
    expect(onSelectedChange).not.toHaveBeenCalled();
  });

  test("paste flow adds acknowledged IPs to the selected set", async () => {
    const onSelectedChange = vi.fn<(next: Set<string>) => void>();
    const user = userEvent.setup();

    renderWithQuery(
      <DestinationPanel
        selected={new Set()}
        onSelectedChange={onSelectedChange}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: /add ips/i }));

    const modal = screen.getByTestId("paste-staging-mock");
    await user.click(within(modal).getByRole("button", { name: /simulate paste success/i }));

    const snapshot = onSelectedChange.mock.calls.at(-1)?.[0];
    expect(Array.from(snapshot ?? [])).toContain("1.2.3.4");
  });

  test("`~` prefix visible iff filter.shapes.length > 0", () => {
    const { rerender } = renderWithQuery(
      <DestinationPanel
        selected={new Set()}
        onSelectedChange={vi.fn()}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );
    expect(screen.getByText(/Estimated total: 2/)).toBeInTheDocument();
    expect(screen.queryByText(/Estimated total: ~/)).not.toBeInTheDocument();

    const shapeFilter: FilterValue = {
      ...EMPTY_FILTER,
      shapes: [{ kind: "rectangle", sw: [10, 50], ne: [15, 55] }],
    };
    rerender(
      <DestinationPanel
        selected={new Set()}
        onSelectedChange={vi.fn()}
        filter={shapeFilter}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );
    expect(screen.getByText(/Estimated total: ~2/)).toBeInTheDocument();
  });

  test("does not expose a duplicate 'Add matching' button", () => {
    renderWithQuery(
      <DestinationPanel
        selected={new Set()}
        onSelectedChange={vi.fn()}
        filter={EMPTY_FILTER}
        onFilterChange={vi.fn()}
        facets={undefined}
        onOpenMap={vi.fn()}
      />,
    );
    expect(screen.queryByRole("button", { name: /add matching/i })).not.toBeInTheDocument();
  });

  describe("hostname rendering", () => {
    test("wraps the catalogue IP cell in <IpHostname>, appending `(hostname)` when seeded", async () => {
      mockList({ pages: pagesOf([[ENTRY_A]]) });

      renderPanel(
        <DestinationPanel
          selected={new Set()}
          onSelectedChange={vi.fn()}
          filter={EMPTY_FILTER}
          onFilterChange={vi.fn()}
          facets={undefined}
          onOpenMap={vi.fn()}
        />,
        [{ ip: "10.0.0.1", hostname: "alpha.example.com" }],
      );

      expect(await screen.findByText("(alpha.example.com)")).toBeInTheDocument();
      expect(screen.getByText("10.0.0.1, hostname alpha.example.com")).toBeInTheDocument();
    });

    test("falls back to the bare IP when the provider has no hit", () => {
      mockList({ pages: pagesOf([[ENTRY_A]]) });

      renderPanel(
        <DestinationPanel
          selected={new Set()}
          onSelectedChange={vi.fn()}
          filter={EMPTY_FILTER}
          onFilterChange={vi.fn()}
          facets={undefined}
          onOpenMap={vi.fn()}
        />,
      );

      expect(screen.getByText("10.0.0.1")).toBeInTheDocument();
      expect(screen.queryByText(/hostname/)).not.toBeInTheDocument();
    });
  });
});
