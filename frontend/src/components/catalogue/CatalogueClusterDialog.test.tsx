import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import type { CatalogueEntry, CatalogueListResponse } from "@/api/hooks/catalogue";
import { renderWithQuery } from "@/test/query-wrapper";

// Mock the hook before importing the component so the component's
// imported reference resolves to our spy.
vi.mock("@/api/hooks/catalogue", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/catalogue")>("@/api/hooks/catalogue");
  return {
    ...actual,
    useCatalogueListInfinite: vi.fn(),
  };
});

import { useCatalogueListInfinite } from "@/api/hooks/catalogue";
import { CatalogueClusterDialog } from "@/components/catalogue/CatalogueClusterDialog";
import type { Bbox } from "@/lib/geo";

const mockUseCatalogueListInfinite = vi.mocked(useCatalogueListInfinite);

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

function buildPage(entries: CatalogueEntry[], total: number): CatalogueListResponse {
  return { entries, next_cursor: null, total };
}

interface InfiniteResultShape {
  data: { pages: CatalogueListResponse[] } | undefined;
  hasNextPage: boolean;
  isFetchingNextPage: boolean;
  fetchNextPage: () => void;
  isError: boolean;
}

/**
 * Mount the infinite-query mock return-value around the minimum surface
 * the dialog reads. Tests only need `data.pages`, `hasNextPage`,
 * `isFetchingNextPage`, `fetchNextPage`, and `isError`; the rest of the
 * react-query return value is cast through `unknown` since the spec
 * exceeds this test's boundary.
 */
function mockInfinite(result: Partial<InfiniteResultShape>): void {
  const full: InfiniteResultShape = {
    data: undefined,
    hasNextPage: false,
    isFetchingNextPage: false,
    fetchNextPage: vi.fn(),
    isError: false,
    ...result,
  };
  // biome-ignore lint/suspicious/noExplicitAny: test double
  mockUseCatalogueListInfinite.mockReturnValue(full as any);
}

const CELL: Bbox = [10, 20, 11, 21];

describe("CatalogueClusterDialog", () => {
  test("renders nothing while closed", () => {
    mockInfinite({});
    renderWithQuery(
      <CatalogueClusterDialog
        open={false}
        onOpenChange={() => {}}
        cell={null}
        filters={{}}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  test("first page of 50 (total 127) shows 'Showing 50 of 127' and enables Load more", () => {
    const entries = Array.from({ length: 50 }, (_, i) =>
      makeEntry({ id: `e${i}`, ip: `10.0.0.${i}` }),
    );
    mockInfinite({
      data: { pages: [buildPage(entries, 127)] },
      hasNextPage: true,
    });
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        cell={CELL}
        filters={{}}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.getByText("Showing 50 of 127 in this area")).toBeInTheDocument();
    const loadMoreButton = screen.getByRole("button", { name: /^load more$/i });
    expect(loadMoreButton).toBeEnabled();
  });

  test("two pages (50 + 50 of 127) show 'Showing 100 of 127' and keep Load more enabled", () => {
    const first = Array.from({ length: 50 }, (_, i) =>
      makeEntry({ id: `a${i}`, ip: `10.0.0.${i}` }),
    );
    const second = Array.from({ length: 50 }, (_, i) =>
      makeEntry({ id: `b${i}`, ip: `10.0.1.${i}` }),
    );
    mockInfinite({
      data: { pages: [buildPage(first, 127), buildPage(second, 127)] },
      hasNextPage: true,
    });
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        cell={CELL}
        filters={{}}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.getByText("Showing 100 of 127 in this area")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^load more$/i })).toBeEnabled();
  });

  test("all pages loaded (127 of 127): Load more disabled and labelled 'All loaded'", () => {
    const first = Array.from({ length: 50 }, (_, i) => makeEntry({ id: `a${i}` }));
    const second = Array.from({ length: 50 }, (_, i) => makeEntry({ id: `b${i}` }));
    const third = Array.from({ length: 27 }, (_, i) => makeEntry({ id: `c${i}` }));
    mockInfinite({
      data: {
        pages: [buildPage(first, 127), buildPage(second, 127), buildPage(third, 127)],
      },
      hasNextPage: false,
    });
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        cell={CELL}
        filters={{}}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.getByText("Showing 127 of 127 in this area")).toBeInTheDocument();
    const button = screen.getByRole("button", { name: /all loaded/i });
    expect(button).toBeDisabled();
  });

  test("'Loading…' label shown while fetching next page", () => {
    const entries = Array.from({ length: 50 }, (_, i) => makeEntry({ id: `e${i}` }));
    mockInfinite({
      data: { pages: [buildPage(entries, 127)] },
      hasNextPage: true,
      isFetchingNextPage: true,
    });
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        cell={CELL}
        filters={{}}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.getByRole("button", { name: /loading…/i })).toBeDisabled();
  });

  test("Load more click fires fetchNextPage", async () => {
    const fetchNextPage = vi.fn();
    const entries = [makeEntry({ id: "e1" })];
    mockInfinite({
      data: { pages: [buildPage(entries, 2)] },
      hasNextPage: true,
      fetchNextPage,
    });
    const user = userEvent.setup();
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        cell={CELL}
        filters={{}}
        onOpenEntry={() => {}}
      />,
    );
    await user.click(screen.getByRole("button", { name: /^load more$/i }));
    expect(fetchNextPage).toHaveBeenCalledOnce();
  });

  test("hook is called with enabled: false when cell is null", () => {
    mockInfinite({});
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        cell={null}
        filters={{}}
        onOpenEntry={() => {}}
      />,
    );
    expect(mockUseCatalogueListInfinite).toHaveBeenCalled();
    const [, options] = mockUseCatalogueListInfinite.mock.calls.at(-1) ?? [];
    expect(options).toMatchObject({ enabled: false });
  });

  test("hook is called with enabled: false when open is false", () => {
    mockInfinite({});
    renderWithQuery(
      <CatalogueClusterDialog
        open={false}
        onOpenChange={() => {}}
        cell={CELL}
        filters={{}}
        onOpenEntry={() => {}}
      />,
    );
    const [, options] = mockUseCatalogueListInfinite.mock.calls.at(-1) ?? [];
    expect(options).toMatchObject({ enabled: false });
  });

  test("hook is called with enabled: true + cell bbox + filters when open", () => {
    mockInfinite({});
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        cell={CELL}
        filters={{ country_code: ["DE"], asn: [64500] }}
        onOpenEntry={() => {}}
      />,
    );
    const [query, options] = mockUseCatalogueListInfinite.mock.calls.at(-1) ?? [];
    expect(options).toMatchObject({ enabled: true, pageSize: 50 });
    expect(query).toMatchObject({
      bbox: CELL,
      country_code: ["DE"],
      asn: [64500],
    });
  });

  test("row click fires onOpenEntry and closes the dialog", async () => {
    const onOpenEntry = vi.fn();
    const onOpenChange = vi.fn();
    const entries = [
      makeEntry({ id: "e1", display_name: "Alpha" }),
      makeEntry({ id: "e2", display_name: "Beta" }),
    ];
    mockInfinite({
      data: { pages: [buildPage(entries, 2)] },
    });
    const user = userEvent.setup();
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={onOpenChange}
        cell={CELL}
        filters={{}}
        onOpenEntry={onOpenEntry}
      />,
    );
    const button = screen.getByRole("button", { name: /open details for Beta/i });
    await user.click(button);
    expect(onOpenEntry).toHaveBeenCalledWith("e2");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  test("renders one row per entry using the shared EntryCard info block", () => {
    const entries = [
      makeEntry({
        id: "e1",
        ip: "1.1.1.1",
        display_name: "Alpha",
        asn: 12345,
        network_operator: "AcmeNet",
        city: "Paris",
        country_name: "France",
      }),
      makeEntry({ id: "e2", ip: "2.2.2.2", display_name: "Beta" }),
    ];
    mockInfinite({
      data: { pages: [buildPage(entries, 2)] },
    });
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        cell={CELL}
        filters={{}}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.getByText("Alpha")).toBeInTheDocument();
    expect(screen.getByText("Beta")).toBeInTheDocument();
    expect(screen.getByText("Paris, France")).toBeInTheDocument();
    expect(screen.getByText("AS12345 · AcmeNet")).toBeInTheDocument();
  });
});
