import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { CatalogueClusterDialog } from "@/components/catalogue/CatalogueClusterDialog";
import { renderWithQuery } from "@/test/query-wrapper";

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

describe("CatalogueClusterDialog", () => {
  test("does not render content when closed", () => {
    renderWithQuery(
      <CatalogueClusterDialog
        open={false}
        onOpenChange={() => {}}
        entries={[makeEntry()]}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  test("renders title with pin count (plural)", () => {
    const entries = [
      makeEntry({ id: "e1", ip: "1.1.1.1" }),
      makeEntry({ id: "e2", ip: "2.2.2.2" }),
      makeEntry({ id: "e3", ip: "3.3.3.3" }),
    ];
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        entries={entries}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.getByText("3 pins in this area")).toBeInTheDocument();
  });

  test("renders title with pin count (singular)", () => {
    const entries = [makeEntry({ id: "e1", ip: "1.1.1.1" })];
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        entries={entries}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.getByText("1 pin in this area")).toBeInTheDocument();
  });

  test("clicking a row fires onOpenEntry with the row id and closes the dialog", async () => {
    const onOpenEntry = vi.fn();
    const onOpenChange = vi.fn();
    const entries = [
      makeEntry({ id: "e1", ip: "1.1.1.1", display_name: "Alpha" }),
      makeEntry({ id: "e2", ip: "2.2.2.2", display_name: "Beta" }),
    ];
    const user = userEvent.setup();
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={onOpenChange}
        entries={entries}
        onOpenEntry={onOpenEntry}
      />,
    );
    const button = screen.getByRole("button", { name: /open details for Beta/i });
    await user.click(button);
    expect(onOpenEntry).toHaveBeenCalledWith("e2");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  test("renders one row per entry with the shared EntryCard info block", () => {
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
    renderWithQuery(
      <CatalogueClusterDialog
        open={true}
        onOpenChange={() => {}}
        entries={entries}
        onOpenEntry={() => {}}
      />,
    );
    expect(screen.getByText("Alpha")).toBeInTheDocument();
    expect(screen.getByText("Beta")).toBeInTheDocument();
    expect(screen.getByText("Paris, France")).toBeInTheDocument();
    expect(screen.getByText("AS12345 · AcmeNet")).toBeInTheDocument();
  });
});
