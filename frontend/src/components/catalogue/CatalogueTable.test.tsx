import { fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { renderWithProviders } from "@/test/query-wrapper";
import {
  CatalogueTable,
  type CatalogueTableProps,
  type CatalogueTableSort,
  formatWebsiteHost,
  LS_KEY,
  ROW_HEIGHT_ESTIMATE,
} from "./CatalogueTable";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const ENTRY_A: CatalogueEntry = {
  id: "fixture-id-a",
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
  website: "https://example.com",
  notes: "test note",
};

const ENTRY_B: CatalogueEntry = {
  id: "fixture-id-b",
  ip: "5.6.7.8",
  display_name: "Beta Node",
  city: "Berlin",
  country_code: "DE",
  country_name: "Germany",
  asn: 5678,
  network_operator: "Other ISP",
  enrichment_status: "failed",
  operator_edited_fields: ["DisplayName"],
  created_at: "2026-01-02T00:00:00Z",
  source: "operator",
  latitude: 52.52,
  longitude: 13.4,
};

const ENTRIES = [ENTRY_A, ENTRY_B];

const UNSORTED: CatalogueTableSort = { col: null, dir: null };

type PartialProps = Partial<CatalogueTableProps>;

/** Build a full props object with sensible defaults for the table. */
function buildProps(overrides: PartialProps = {}): CatalogueTableProps {
  return {
    rows: ENTRIES,
    total: ENTRIES.length,
    hasNextPage: false,
    isFetchingNextPage: false,
    fetchNextPage: vi.fn(),
    sort: UNSORTED,
    onSortChange: vi.fn(),
    onRowClick: vi.fn(),
    onReenrich: vi.fn(),
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// Setup / teardown
// ---------------------------------------------------------------------------

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("CatalogueTable", () => {
  describe("default columns visible", () => {
    test("renders the expected column headers", async () => {
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      // SortableHeader wraps each sortable column label in a button, so we
      // look up the text inside the columnheader cell rather than rely on
      // accessible-name matching.
      expect(await screen.findByRole("columnheader", { name: /IP/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Name/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /City/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Country/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /ASN/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Network/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Status/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Actions/ })).toBeInTheDocument();
    });

    test("renders IP and display_name cell values", async () => {
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      expect(await screen.findByText("1.2.3.4")).toBeInTheDocument();
      expect(screen.getByText("Alpha Node")).toBeInTheDocument();
      expect(screen.getByText("5.6.7.8")).toBeInTheDocument();
      expect(screen.getByText("Beta Node")).toBeInTheDocument();
    });

    test("renders country name in Country column (resolved from code)", async () => {
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      expect(await screen.findByText("Netherlands")).toBeInTheDocument();
      expect(screen.getByText("Germany")).toBeInTheDocument();
    });

    test("renders em-dash when display_name is null", async () => {
      const entry: CatalogueEntry = {
        ...ENTRY_A,
        id: "fixture-id-null",
        display_name: null,
      };
      renderWithProviders(<CatalogueTable {...buildProps({ rows: [entry], total: 1 })} />);

      await screen.findByText("1.2.3.4");
      expect(screen.getByText("—")).toBeInTheDocument();
    });

    test("renders em-dash when network_operator is null", async () => {
      const entry: CatalogueEntry = {
        ...ENTRY_A,
        id: "fixture-id-null-net",
        network_operator: null,
      };
      renderWithProviders(<CatalogueTable {...buildProps({ rows: [entry], total: 1 })} />);

      await screen.findByText("1.2.3.4");
      expect(screen.getByText("—")).toBeInTheDocument();
    });
  });

  describe("row click fires onRowClick(id)", () => {
    test("clicking a row calls onRowClick with the row id", async () => {
      const onRowClick = vi.fn();
      renderWithProviders(<CatalogueTable {...buildProps({ onRowClick })} />);

      const row = await screen.findByRole("button", { name: /Open entry 1\.2\.3\.4/i });
      fireEvent.click(row);

      expect(onRowClick).toHaveBeenCalledOnce();
      expect(onRowClick).toHaveBeenCalledWith("fixture-id-a");
    });

    test("pressing Enter on a focused row calls onRowClick", async () => {
      const user = userEvent.setup();
      const onRowClick = vi.fn();
      renderWithProviders(<CatalogueTable {...buildProps({ onRowClick })} />);

      const row = await screen.findByRole("button", { name: /Open entry 1\.2\.3\.4/i });
      row.focus();
      await user.keyboard("{Enter}");

      expect(onRowClick).toHaveBeenCalledOnce();
      expect(onRowClick).toHaveBeenCalledWith("fixture-id-a");
    });

    test("pressing Space on a focused row calls onRowClick", async () => {
      const user = userEvent.setup();
      const onRowClick = vi.fn();
      renderWithProviders(<CatalogueTable {...buildProps({ onRowClick })} />);

      const row = await screen.findByRole("button", { name: /Open entry 1\.2\.3\.4/i });
      row.focus();
      await user.keyboard(" ");

      expect(onRowClick).toHaveBeenCalledOnce();
      expect(onRowClick).toHaveBeenCalledWith("fixture-id-a");
    });

    test("rows expose button role, tabIndex=0, and aria-label", async () => {
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      const rowA = await screen.findByRole("button", { name: /Open entry 1\.2\.3\.4/i });
      expect(rowA).toHaveAttribute("tabindex", "0");

      const rowB = screen.getByRole("button", { name: /Open entry 5\.6\.7\.8/i });
      expect(rowB).toHaveAttribute("tabindex", "0");
    });
  });

  describe("re-enrich button fires onReenrich(id)", () => {
    test("clicking the Actions re-enrich icon button calls onReenrich with the row id", async () => {
      const onReenrich = vi.fn();
      renderWithProviders(<CatalogueTable {...buildProps({ onReenrich })} />);

      const reenrichBtn = await screen.findByRole("button", { name: "Re-enrich 1.2.3.4" });
      fireEvent.click(reenrichBtn);

      expect(onReenrich).toHaveBeenCalledOnce();
      expect(onReenrich).toHaveBeenCalledWith("fixture-id-a");
    });

    test("clicking re-enrich does NOT fire onRowClick (stopPropagation)", async () => {
      const onRowClick = vi.fn();
      const onReenrich = vi.fn();
      renderWithProviders(<CatalogueTable {...buildProps({ onRowClick, onReenrich })} />);

      const reenrichBtn = await screen.findByRole("button", { name: "Re-enrich 1.2.3.4" });
      fireEvent.click(reenrichBtn);

      expect(onReenrich).toHaveBeenCalledOnce();
      expect(onRowClick).not.toHaveBeenCalled();
    });

    test("ENTRY_B with operator_edited_fields shows failed status chip and Actions re-enrich", async () => {
      const onReenrich = vi.fn();
      renderWithProviders(
        <CatalogueTable {...buildProps({ rows: [ENTRY_B], total: 1, onReenrich })} />,
      );

      expect(await screen.findByText("Failed")).toBeInTheDocument();
      expect(screen.queryByLabelText("Operator-edited")).not.toBeInTheDocument();

      const reenrichBtn = screen.getByRole("button", { name: "Re-enrich 5.6.7.8" });
      fireEvent.click(reenrichBtn);
      expect(onReenrich).toHaveBeenCalledWith("fixture-id-b");
    });
  });

  describe("column chooser persists per-operator in localStorage", () => {
    test("toggling a column off writes to localStorage", async () => {
      const user = userEvent.setup();
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      const chooserBtn = await screen.findByRole("button", { name: /columns/i });
      await user.click(chooserBtn);

      const cityCheckbox = screen.getByRole("checkbox", { name: /city/i });
      expect(cityCheckbox).toBeChecked();
      await user.click(cityCheckbox);
      expect(cityCheckbox).not.toBeChecked();

      const stored = localStorage.getItem(LS_KEY);
      expect(stored).not.toBeNull();
      // biome-ignore lint/style/noNonNullAssertion: guarded by expect above
      const parsed: unknown = JSON.parse(stored!);
      expect(parsed).toEqual(expect.any(Object));
      expect(Array.isArray(parsed)).toBe(false);
      expect(parsed).toMatchObject({ city: false, ip: true, display_name: true });
    });

    test("hidden column persists across remount (localStorage restore)", async () => {
      const user = userEvent.setup();
      const { unmount } = renderWithProviders(<CatalogueTable {...buildProps()} />);

      const chooserBtn = await screen.findByRole("button", { name: /columns/i });
      await user.click(chooserBtn);
      const cityCheckbox = screen.getByRole("checkbox", { name: /city/i });
      await user.click(cityCheckbox);

      expect(screen.queryByRole("columnheader", { name: /City/ })).not.toBeInTheDocument();

      unmount();
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      await screen.findByRole("columnheader", { name: /IP/ });
      expect(screen.queryByRole("columnheader", { name: /City/ })).not.toBeInTheDocument();
    });

    test("optional columns are off by default (Location not visible initially)", async () => {
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      await screen.findByRole("columnheader", { name: /IP/ });
      expect(screen.queryByRole("columnheader", { name: /Location/ })).not.toBeInTheDocument();
    });

    test("optional column can be toggled on via chooser", async () => {
      const user = userEvent.setup();
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      const chooserBtn = await screen.findByRole("button", { name: /columns/i });
      await user.click(chooserBtn);

      const locationCheckbox = screen.getByRole("checkbox", { name: /location/i });
      expect(locationCheckbox).not.toBeChecked();
      await user.click(locationCheckbox);

      expect(screen.getByRole("columnheader", { name: /Location/ })).toBeInTheDocument();
    });

    test("Location cell renders Present when both lat and lon exist, Unset otherwise", async () => {
      const user = userEvent.setup();
      const entryWithCoords = ENTRY_A;
      const entryWithoutCoords: CatalogueEntry = {
        ...ENTRY_A,
        id: "fixture-id-c",
        ip: "9.9.9.9",
        latitude: null,
        longitude: null,
      };
      renderWithProviders(
        <CatalogueTable
          {...buildProps({ rows: [entryWithCoords, entryWithoutCoords], total: 2 })}
        />,
      );

      const chooserBtn = await screen.findByRole("button", { name: /columns/i });
      await user.click(chooserBtn);
      await user.click(screen.getByRole("checkbox", { name: /location/i }));

      expect(screen.getByText("Present")).toBeInTheDocument();
      expect(screen.getByText("Unset")).toBeInTheDocument();
    });

    test("first-time users (no stored preferences) see compile-time default columns", async () => {
      expect(localStorage.getItem(LS_KEY)).toBeNull();

      renderWithProviders(<CatalogueTable {...buildProps()} />);

      await screen.findByRole("columnheader", { name: /IP/ });
      expect(screen.getByRole("columnheader", { name: /Actions/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Status/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Network/ })).toBeInTheDocument();
    });

    test("newly added default-visible column stays visible for users with older saved preferences", async () => {
      const legacyMap: Record<string, boolean> = {
        ip: true,
        display_name: true,
        city: true,
        country: true,
        asn: true,
        network: true,
        status: true,
      };
      localStorage.setItem(LS_KEY, JSON.stringify(legacyMap));

      renderWithProviders(<CatalogueTable {...buildProps()} />);

      await screen.findByRole("columnheader", { name: /IP/ });
      expect(screen.getByRole("columnheader", { name: /Actions/ })).toBeInTheDocument();
    });

    test("legacy array-shaped localStorage payload is ignored and defaults apply", async () => {
      const legacyArray = ["ip", "display_name"];
      localStorage.setItem(LS_KEY, JSON.stringify(legacyArray));

      renderWithProviders(<CatalogueTable {...buildProps()} />);

      await screen.findByRole("columnheader", { name: /IP/ });
      expect(screen.getByRole("columnheader", { name: /City/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Country/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /ASN/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Network/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Status/ })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Actions/ })).toBeInTheDocument();

      const stored = localStorage.getItem(LS_KEY);
      expect(stored).not.toBeNull();
      // biome-ignore lint/style/noNonNullAssertion: guarded by expect above
      const parsed: unknown = JSON.parse(stored!);
      expect(Array.isArray(parsed)).toBe(false);
      expect(parsed).toEqual(expect.any(Object));
    });

    test("Actions column is always the rightmost header with all optional columns enabled", async () => {
      const user = userEvent.setup();
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      const chooserBtn = await screen.findByRole("button", { name: /columns/i });
      await user.click(chooserBtn);
      for (const label of ["Location", "Website", "Notes"]) {
        const checkbox = screen.getByRole("checkbox", { name: new RegExp(label, "i") });
        if (!checkbox.hasAttribute("checked") && !(checkbox as HTMLInputElement).checked) {
          await user.click(checkbox);
        }
      }

      const headers = screen.getAllByRole("columnheader");
      const headerNames = headers.map((h) => h.textContent?.trim() ?? "");
      // The last header contains "Actions" (plain text, no sort button).
      expect(headerNames.at(-1)).toBe("Actions");
    });
  });

  // -----------------------------------------------------------------------
  // Sortable headers
  // -----------------------------------------------------------------------

  describe("sortable headers", () => {
    test("clicking an unsorted column header sends (col, 'asc')", async () => {
      const onSortChange = vi.fn();
      renderWithProviders(<CatalogueTable {...buildProps({ onSortChange })} />);

      // Each sortable header renders a button with text-label content.
      const ipHeaderButton = await screen.findByRole("button", { name: /^IP/ });
      fireEvent.click(ipHeaderButton);
      expect(onSortChange).toHaveBeenCalledWith("ip", "asc");
    });

    test("clicking again while ascending sends (col, 'desc')", async () => {
      const onSortChange = vi.fn();
      const sort: CatalogueTableSort = { col: "ip", dir: "asc" };
      renderWithProviders(<CatalogueTable {...buildProps({ sort, onSortChange })} />);

      const ipHeaderButton = await screen.findByRole("button", { name: /^IP/ });
      fireEvent.click(ipHeaderButton);
      expect(onSortChange).toHaveBeenCalledWith("ip", "desc");
    });

    test("clicking again while descending returns to unsorted (null, null)", async () => {
      const onSortChange = vi.fn();
      const sort: CatalogueTableSort = { col: "ip", dir: "desc" };
      renderWithProviders(<CatalogueTable {...buildProps({ sort, onSortChange })} />);

      const ipHeaderButton = await screen.findByRole("button", { name: /^IP/ });
      fireEvent.click(ipHeaderButton);
      expect(onSortChange).toHaveBeenCalledWith(null, null);
    });

    test("aria-sort reflects the current sort state on the active column", async () => {
      const sort: CatalogueTableSort = { col: "ip", dir: "asc" };
      renderWithProviders(<CatalogueTable {...buildProps({ sort })} />);

      // aria-sort sits on the <th> (columnheader role) — the enclosing
      // cell for each sortable column.
      const ipHeader = await screen.findByRole("columnheader", { name: /^IP/ });
      expect(ipHeader).toHaveAttribute("aria-sort", "ascending");

      const nameHeader = screen.getByRole("columnheader", { name: /^Name/ });
      expect(nameHeader).toHaveAttribute("aria-sort", "none");
    });

    test("aria-sort flips to descending when dir is 'desc'", async () => {
      const sort: CatalogueTableSort = { col: "ip", dir: "desc" };
      renderWithProviders(<CatalogueTable {...buildProps({ sort })} />);

      const ipHeader = await screen.findByRole("columnheader", { name: /^IP/ });
      expect(ipHeader).toHaveAttribute("aria-sort", "descending");
    });

    test("country header sort maps to the backend 'country_code' column", async () => {
      const onSortChange = vi.fn();
      renderWithProviders(<CatalogueTable {...buildProps({ onSortChange })} />);

      const countryButton = await screen.findByRole("button", { name: /^Country/ });
      fireEvent.click(countryButton);
      expect(onSortChange).toHaveBeenCalledWith("country_code", "asc");
    });

    test("Actions column header is plain text (not sortable)", async () => {
      renderWithProviders(<CatalogueTable {...buildProps()} />);

      const actionsHeader = await screen.findByRole("columnheader", { name: /Actions/ });
      // No sort button inside the Actions header cell.
      const buttons = actionsHeader.querySelectorAll("button");
      expect(buttons.length).toBe(0);
    });
  });

  // -----------------------------------------------------------------------
  // Load-more + total counter
  // -----------------------------------------------------------------------

  describe("Load-more and total counter", () => {
    test("shows the server-reported total count next to the Load-more control", async () => {
      renderWithProviders(<CatalogueTable {...buildProps({ total: 327 })} />);

      expect(await screen.findByText(/2 of 327 entries/)).toBeInTheDocument();
    });

    test("Load-more button is disabled when hasNextPage is false", async () => {
      renderWithProviders(<CatalogueTable {...buildProps({ hasNextPage: false })} />);

      const button = await screen.findByRole("button", { name: /all loaded/i });
      expect(button).toBeDisabled();
    });

    test("Load-more button is enabled and labelled 'Load more' when hasNextPage is true", async () => {
      renderWithProviders(<CatalogueTable {...buildProps({ hasNextPage: true })} />);

      const button = await screen.findByRole("button", { name: /^load more$/i });
      expect(button).toBeEnabled();
    });

    test("Load-more button shows 'Loading…' while isFetchingNextPage", async () => {
      renderWithProviders(
        <CatalogueTable {...buildProps({ hasNextPage: true, isFetchingNextPage: true })} />,
      );

      const button = await screen.findByRole("button", { name: /loading…/i });
      expect(button).toBeDisabled();
    });

    test("clicking Load-more invokes fetchNextPage", async () => {
      const fetchNextPage = vi.fn();
      renderWithProviders(<CatalogueTable {...buildProps({ hasNextPage: true, fetchNextPage })} />);

      const button = await screen.findByRole("button", { name: /^load more$/i });
      fireEvent.click(button);
      expect(fetchNextPage).toHaveBeenCalledOnce();
    });
  });

  // -----------------------------------------------------------------------
  // Virtualization smoke test
  // -----------------------------------------------------------------------

  describe("virtualization", () => {
    test("renders without hanging with 2000 rows and keeps the header reachable", async () => {
      // jsdom doesn't compute layout, so the virtualizer may render a
      // conservative default window. We assert the weaker invariant: the
      // table mounts, the header renders, and the DOM row count stays
      // finite (data-index rows are << rows.length).
      const rows: CatalogueEntry[] = Array.from({ length: 2000 }, (_, i) => ({
        ...ENTRY_A,
        id: `bulk-${i}`,
        ip: `10.${Math.floor(i / 256)}.${i % 256}.1`,
        display_name: `Bulk ${i}`,
      }));
      renderWithProviders(<CatalogueTable {...buildProps({ rows, total: rows.length })} />);

      await screen.findByRole("columnheader", { name: /IP/ });

      const rendered = document.querySelectorAll("tr[data-index]");
      // With a 2000-row dataset, the virtualizer should never commit all
      // of them at once regardless of layout measurements.
      expect(rendered.length).toBeLessThan(2000);
    });

    test("rows with very long cell content keep the fixed virtualizer row height", async () => {
      // Regression guard for the T51 layout bug: a row carrying a
      // massively-long notes string would grow past the 44px the
      // virtualizer pinned in its translateY math, causing neighbouring
      // rows to overlap visually. The fix truncates every cell to a
      // single line, so the inline style on the `<tr>` must always
      // report exactly `ROW_HEIGHT_ESTIMATE`.
      const longNotes = "qwdqwo;as ".repeat(50);
      const longCity = "A really very absurdly long city name ".repeat(10);
      const rows: CatalogueEntry[] = [
        {
          ...ENTRY_A,
          id: "long-row",
          ip: "8.8.8.8",
          notes: longNotes,
          city: longCity,
        },
      ];
      const user = userEvent.setup();
      renderWithProviders(<CatalogueTable {...buildProps({ rows, total: 1 })} />);

      // Enable notes so the long cell is actually in the DOM.
      const chooserBtn = await screen.findByRole("button", { name: /columns/i });
      await user.click(chooserBtn);
      await user.click(screen.getByRole("checkbox", { name: /notes/i }));

      const renderedRow = await screen.findByRole("button", { name: /Open entry 8\.8\.8\.8/i });
      expect(renderedRow.style.height).toBe(`${ROW_HEIGHT_ESTIMATE}px`);
    });

    test("rendered column widths match the declared explicit widths", async () => {
      // Fixed table layout + shared colgroup is how header and body
      // stay aligned. Verify both tables carry the same <col>
      // width for each visible column id.
      const { container } = renderWithProviders(<CatalogueTable {...buildProps()} />);
      await screen.findByRole("columnheader", { name: /IP/ });

      const tables = container.querySelectorAll("table");
      // Two tables: header + virtualized body.
      expect(tables.length).toBe(2);

      const widthsPerTable = Array.from(tables).map((t) =>
        Array.from(t.querySelectorAll("colgroup > col")).map((c) => (c as HTMLElement).style.width),
      );

      // Header/body colgroups must match column-for-column.
      expect(widthsPerTable[0]).toEqual(widthsPerTable[1]);
      // Every column must have an explicit px width (no auto-sizing).
      for (const width of widthsPerTable[0]) {
        expect(width).toMatch(/^\d+px$/);
      }
    });
  });
});

// ---------------------------------------------------------------------------
// formatWebsiteHost unit tests
// ---------------------------------------------------------------------------

describe("formatWebsiteHost", () => {
  test("extracts hostname from a full URL with path and query", () => {
    expect(formatWebsiteHost("https://google.com/blah/blah?ssd&asdads=10")).toBe("google.com");
  });

  test("extracts hostname from a URL without a path", () => {
    expect(formatWebsiteHost("https://example.com")).toBe("example.com");
  });

  test("handles URLs without a scheme by prepending https://", () => {
    expect(formatWebsiteHost("example.com")).toBe("example.com");
  });

  test("returns the raw string when parsing fails (truly malformed input)", () => {
    expect(formatWebsiteHost("not a url at all ://???")).toBe("not a url at all ://???");
  });

  test("strips fragment from URL", () => {
    expect(formatWebsiteHost("https://example.com/page#section")).toBe("example.com");
  });
});

// ---------------------------------------------------------------------------
// Website column integration test
// ---------------------------------------------------------------------------

describe("website column display", () => {
  test("shows only the hostname in the anchor text while preserving href and title", async () => {
    const user = userEvent.setup();
    const entry: CatalogueEntry = {
      ...ENTRY_A,
      id: "website-test",
      website: "https://google.com/blah/blah?ssd",
    };
    renderWithProviders(<CatalogueTable {...buildProps({ rows: [entry], total: 1 })} />);

    const chooserBtn = await screen.findByRole("button", { name: /columns/i });
    await user.click(chooserBtn);
    const websiteCheckbox = screen.getByRole("checkbox", { name: /website/i });
    await user.click(websiteCheckbox);

    const link = await screen.findByRole("link");
    expect(link).toHaveTextContent("google.com");
    expect(link).toHaveAttribute("href", "https://google.com/blah/blah?ssd");
    expect(link).toHaveAttribute("title", "https://google.com/blah/blah?ssd");
  });
});
