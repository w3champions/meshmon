import { fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { renderWithProviders } from "@/test/query-wrapper";
import { CatalogueTable, LS_KEY } from "./CatalogueTable";

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
      const onRowClick = vi.fn();
      const onReenrich = vi.fn();
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={onRowClick} onReenrich={onReenrich} />,
      );

      expect(await screen.findByRole("columnheader", { name: "IP" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Name" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "City" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Country" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "ASN" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Network" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Status" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Actions" })).toBeInTheDocument();
    });

    test("renders IP and display_name cell values", async () => {
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      expect(await screen.findByText("1.2.3.4")).toBeInTheDocument();
      expect(screen.getByText("Alpha Node")).toBeInTheDocument();
      expect(screen.getByText("5.6.7.8")).toBeInTheDocument();
      expect(screen.getByText("Beta Node")).toBeInTheDocument();
    });

    test("renders country code in Country column", async () => {
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      expect(await screen.findByText("NL")).toBeInTheDocument();
      expect(screen.getByText("DE")).toBeInTheDocument();
    });

    test("renders em-dash when display_name is null", async () => {
      const entry: CatalogueEntry = {
        ...ENTRY_A,
        id: "fixture-id-null",
        display_name: null,
      };
      renderWithProviders(
        <CatalogueTable entries={[entry]} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      await screen.findByText("1.2.3.4");
      expect(screen.getByText("—")).toBeInTheDocument();
    });

    test("renders em-dash when network_operator is null", async () => {
      const entry: CatalogueEntry = {
        ...ENTRY_A,
        id: "fixture-id-null-net",
        network_operator: null,
      };
      renderWithProviders(
        <CatalogueTable entries={[entry]} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      await screen.findByText("1.2.3.4");
      // The Network column cell for this row should show an em-dash
      const rows = screen.getAllByRole("button");
      expect(rows.length).toBeGreaterThanOrEqual(1);
      expect(screen.getByText("—")).toBeInTheDocument();
    });
  });

  describe("row click fires onRowClick(id)", () => {
    test("clicking a row calls onRowClick with the row id", async () => {
      const onRowClick = vi.fn();
      const onReenrich = vi.fn();
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={onRowClick} onReenrich={onReenrich} />,
      );

      // Find the row for ENTRY_A
      const row = await screen.findByRole("button", { name: /Open entry 1\.2\.3\.4/i });
      fireEvent.click(row);

      expect(onRowClick).toHaveBeenCalledOnce();
      expect(onRowClick).toHaveBeenCalledWith("fixture-id-a");
    });

    test("pressing Enter on a focused row calls onRowClick", async () => {
      const user = userEvent.setup();
      const onRowClick = vi.fn();
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={onRowClick} onReenrich={vi.fn()} />,
      );

      const row = await screen.findByRole("button", { name: /Open entry 1\.2\.3\.4/i });
      row.focus();
      await user.keyboard("{Enter}");

      expect(onRowClick).toHaveBeenCalledOnce();
      expect(onRowClick).toHaveBeenCalledWith("fixture-id-a");
    });

    test("pressing Space on a focused row calls onRowClick", async () => {
      const user = userEvent.setup();
      const onRowClick = vi.fn();
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={onRowClick} onReenrich={vi.fn()} />,
      );

      const row = await screen.findByRole("button", { name: /Open entry 1\.2\.3\.4/i });
      row.focus();
      await user.keyboard(" ");

      expect(onRowClick).toHaveBeenCalledOnce();
      expect(onRowClick).toHaveBeenCalledWith("fixture-id-a");
    });

    test("rows expose button role, tabIndex=0, and aria-label", async () => {
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      const rowA = await screen.findByRole("button", { name: /Open entry 1\.2\.3\.4/i });
      expect(rowA).toHaveAttribute("tabindex", "0");

      const rowB = screen.getByRole("button", { name: /Open entry 5\.6\.7\.8/i });
      expect(rowB).toHaveAttribute("tabindex", "0");
    });
  });

  describe("re-enrich button fires onReenrich(id)", () => {
    test("clicking the Actions re-enrich icon button calls onReenrich with the row id", async () => {
      const onRowClick = vi.fn();
      const onReenrich = vi.fn();
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={onRowClick} onReenrich={onReenrich} />,
      );

      // Actions column renders an icon button with aria-label="Re-enrich {ip}".
      const reenrichBtn = await screen.findByRole("button", { name: "Re-enrich 1.2.3.4" });
      fireEvent.click(reenrichBtn);

      expect(onReenrich).toHaveBeenCalledOnce();
      expect(onReenrich).toHaveBeenCalledWith("fixture-id-a");
    });

    test("clicking re-enrich does NOT fire onRowClick (stopPropagation)", async () => {
      const onRowClick = vi.fn();
      const onReenrich = vi.fn();
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={onRowClick} onReenrich={onReenrich} />,
      );

      const reenrichBtn = await screen.findByRole("button", { name: "Re-enrich 1.2.3.4" });
      fireEvent.click(reenrichBtn);

      expect(onReenrich).toHaveBeenCalledOnce();
      expect(onRowClick).not.toHaveBeenCalled();
    });

    test("ENTRY_B with operator_edited_fields shows operator-locked chip and Actions re-enrich", async () => {
      const onReenrich = vi.fn();
      renderWithProviders(
        <CatalogueTable entries={[ENTRY_B]} onRowClick={vi.fn()} onReenrich={onReenrich} />,
      );

      // operator-locked badge rendered by StatusChip (display-only in the table)
      expect(await screen.findByLabelText("Operator-edited")).toBeInTheDocument();

      // Actions column carries the re-enrich button
      const reenrichBtn = screen.getByRole("button", { name: "Re-enrich 5.6.7.8" });
      fireEvent.click(reenrichBtn);
      expect(onReenrich).toHaveBeenCalledWith("fixture-id-b");
    });
  });

  describe("column chooser persists per-operator in localStorage", () => {
    test("toggling a column off writes to localStorage", async () => {
      const user = userEvent.setup();
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      // Open the column chooser
      const chooserBtn = await screen.findByRole("button", { name: /columns/i });
      await user.click(chooserBtn);

      // Toggle the "City" column off
      const cityCheckbox = screen.getByRole("checkbox", { name: /city/i });
      expect(cityCheckbox).toBeChecked();
      await user.click(cityCheckbox);
      expect(cityCheckbox).not.toBeChecked();

      // localStorage should reflect the change — City should be absent
      const stored = localStorage.getItem(LS_KEY);
      expect(stored).not.toBeNull();
      // biome-ignore lint/style/noNonNullAssertion: guarded by expect above
      const parsed: string[] = JSON.parse(stored!);
      expect(parsed).not.toContain("city");
    });

    test("hidden column persists across remount (localStorage restore)", async () => {
      const user = userEvent.setup();
      const { unmount } = renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      // Open chooser and hide City
      const chooserBtn = await screen.findByRole("button", { name: /columns/i });
      await user.click(chooserBtn);
      const cityCheckbox = screen.getByRole("checkbox", { name: /city/i });
      await user.click(cityCheckbox);

      // Confirm City header was removed
      expect(screen.queryByRole("columnheader", { name: "City" })).not.toBeInTheDocument();

      // Unmount and remount
      unmount();
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      // City column should still be hidden after remount
      await screen.findByRole("columnheader", { name: "IP" });
      expect(screen.queryByRole("columnheader", { name: "City" })).not.toBeInTheDocument();
    });

    test("optional columns are off by default (Latitude not visible initially)", async () => {
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      await screen.findByRole("columnheader", { name: "IP" });
      expect(screen.queryByRole("columnheader", { name: "Latitude" })).not.toBeInTheDocument();
    });

    test("optional column can be toggled on via chooser", async () => {
      const user = userEvent.setup();
      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      const chooserBtn = await screen.findByRole("button", { name: /columns/i });
      await user.click(chooserBtn);

      const latCheckbox = screen.getByRole("checkbox", { name: /latitude/i });
      expect(latCheckbox).not.toBeChecked();
      await user.click(latCheckbox);

      // Now Latitude header should be visible
      expect(screen.getByRole("columnheader", { name: "Latitude" })).toBeInTheDocument();
    });

    test("first-time users (no stored preferences) see compile-time default columns", async () => {
      // No localStorage entry → getInitialVisibility seeds from DEFAULT_VISIBLE /
      // OPTIONAL_COLUMNS. This covers the "new install" case and also guards
      // against the hydration regression where a user who never interacted with
      // the chooser would lose access to newly added default-visible columns.
      expect(localStorage.getItem(LS_KEY)).toBeNull();

      renderWithProviders(
        <CatalogueTable entries={ENTRIES} onRowClick={vi.fn()} onReenrich={vi.fn()} />,
      );

      await screen.findByRole("columnheader", { name: "IP" });
      // Every compile-time default-visible column should be present.
      expect(screen.getByRole("columnheader", { name: "Actions" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Status" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Network" })).toBeInTheDocument();
    });
  });
});
