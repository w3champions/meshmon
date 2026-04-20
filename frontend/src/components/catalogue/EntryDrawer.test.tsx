import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import {
  useDeleteCatalogueEntry,
  usePatchCatalogueEntry,
  useReenrichOne,
} from "@/api/hooks/catalogue";
import { EntryDrawer } from "@/components/catalogue/EntryDrawer";

vi.mock("@/api/hooks/catalogue", () => ({
  usePatchCatalogueEntry: vi.fn(),
  useReenrichOne: vi.fn(),
  useDeleteCatalogueEntry: vi.fn(),
}));

type Mutation = {
  mutate: ReturnType<typeof vi.fn>;
  isPending: boolean;
};

function makeMutation(): Mutation {
  return { mutate: vi.fn(), isPending: false };
}

function wrap() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

const patchMutation = makeMutation();
const reenrichMutation = makeMutation();
const deleteMutation = makeMutation();

const ENTRY: CatalogueEntry = {
  id: "11111111-1111-1111-1111-111111111111",
  ip: "10.0.0.1",
  created_at: "2026-04-16T11:59:00Z",
  created_by: "operator",
  enriched_at: "2026-04-16T12:00:00Z",
  source: "operator",
  enrichment_status: "enriched",
  operator_edited_fields: ["DisplayName"],
  display_name: "Alpha",
  asn: 64500,
  country_code: "DE",
  country_name: "Germany",
  city: "San Francisco",
  latitude: 37.77,
  longitude: -122.42,
  network_operator: "ExampleNet",
  website: "https://example.com",
  notes: "primary anchor",
};

beforeEach(() => {
  patchMutation.mutate.mockReset();
  patchMutation.isPending = false;
  reenrichMutation.mutate.mockReset();
  reenrichMutation.isPending = false;
  deleteMutation.mutate.mockReset();
  deleteMutation.isPending = false;
  vi.mocked(usePatchCatalogueEntry).mockReturnValue(
    patchMutation as unknown as ReturnType<typeof usePatchCatalogueEntry>,
  );
  vi.mocked(useReenrichOne).mockReturnValue(
    reenrichMutation as unknown as ReturnType<typeof useReenrichOne>,
  );
  vi.mocked(useDeleteCatalogueEntry).mockReturnValue(
    deleteMutation as unknown as ReturnType<typeof useDeleteCatalogueEntry>,
  );
});

describe("EntryDrawer", () => {
  test("renders all editable fields when an entry is supplied", () => {
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    expect(screen.getByLabelText("Display name")).toHaveValue("Alpha");
    expect(screen.getByLabelText("ASN")).toHaveValue(64500);
    // Country is now a Radix Select (combobox role), not a free-text input.
    // The trigger should display the selected country name + code.
    const countryTrigger = screen.getByRole("combobox", { name: /country/i });
    expect(countryTrigger).toBeInTheDocument();
    expect(countryTrigger).toHaveTextContent("Germany (DE)");
    expect(screen.getByLabelText("City")).toHaveValue("San Francisco");
    expect(screen.getByLabelText("Latitude")).toHaveValue(37.77);
    expect(screen.getByLabelText("Longitude")).toHaveValue(-122.42);
    expect(screen.getByLabelText("Network operator")).toHaveValue("ExampleNet");
    expect(screen.getByLabelText("Website")).toHaveValue("https://example.com");
    expect(screen.getByLabelText("Notes")).toHaveValue("primary anchor");
  });

  test("shows Revert to auto only for fields in operator_edited_fields", () => {
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    // DisplayName is in operator_edited_fields → Revert visible exactly once.
    const reverts = screen.getAllByRole("button", { name: "Revert to auto" });
    expect(reverts).toHaveLength(1);
  });

  test("saving a dirty field PATCHes only that field", async () => {
    const user = userEvent.setup();
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    const city = screen.getByLabelText("City");
    await user.clear(city);
    await user.type(city, "Berlin");
    await user.click(screen.getByRole("button", { name: "Save" }));

    expect(patchMutation.mutate).toHaveBeenCalledTimes(1);
    const [variables] = patchMutation.mutate.mock.calls[0];
    expect(variables.id).toBe(ENTRY.id);
    expect(variables.patch).toEqual({ city: "Berlin" });
  });

  test("selecting a country PATCHes only country_code (no country_name)", async () => {
    const user = userEvent.setup();
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });

    // Open the country Select (Radix uses a button with role=combobox).
    const countryTrigger = screen.getByRole("combobox", { name: /country/i });
    await user.click(countryTrigger);

    // The portal renders options into document.body.
    const frOption = within(document.body).getByRole("option", { name: /France/ });
    await user.click(frOption);

    await user.click(screen.getByRole("button", { name: "Save" }));

    expect(patchMutation.mutate).toHaveBeenCalledTimes(1);
    const [variables] = patchMutation.mutate.mock.calls[0];
    expect(variables.id).toBe(ENTRY.id);
    // Only country_code — no country_name
    expect(variables.patch).toEqual({ country_code: "FR" });
  });

  test("Revert to auto sends PascalCase field name + nulled column", async () => {
    const user = userEvent.setup();
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    await user.click(screen.getByRole("button", { name: "Revert to auto" }));

    expect(patchMutation.mutate).toHaveBeenCalledTimes(1);
    const [variables] = patchMutation.mutate.mock.calls[0];
    expect(variables.id).toBe(ENTRY.id);
    expect(variables.patch).toEqual({
      revert_to_auto: ["DisplayName"],
      display_name: null,
    });
  });

  test("Re-enrich button calls useReenrichOne", async () => {
    const user = userEvent.setup();
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    await user.click(screen.getByRole("button", { name: "Re-enrich" }));
    expect(reenrichMutation.mutate).toHaveBeenCalledTimes(1);
    expect(reenrichMutation.mutate.mock.calls[0][0]).toBe(ENTRY.id);
  });

  test("Delete flow requires confirmation and closes the drawer on success", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    deleteMutation.mutate.mockImplementation((_id: string, opts?: { onSuccess?: () => void }) => {
      opts?.onSuccess?.();
    });

    render(<EntryDrawer entry={ENTRY} onClose={onClose} />, { wrapper: wrap() });
    await user.click(screen.getByRole("button", { name: "Delete" }));
    // Confirm dialog is visible before mutation fires (nested Dialog portals to body).
    expect(deleteMutation.mutate).not.toHaveBeenCalled();
    // The nested confirm dialog is the second dialog in the document.
    const dialogs = within(document.body).getAllByRole("dialog");
    const confirmDialog = dialogs[dialogs.length - 1];
    await user.click(within(confirmDialog).getByRole("button", { name: "Confirm delete" }));
    expect(deleteMutation.mutate).toHaveBeenCalledTimes(1);
    expect(deleteMutation.mutate.mock.calls[0][0]).toBe(ENTRY.id);
    expect(onClose).toHaveBeenCalled();
  });

  test("Cancelling delete leaves drawer open and skips the mutation", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<EntryDrawer entry={ENTRY} onClose={onClose} />, { wrapper: wrap() });
    await user.click(screen.getByRole("button", { name: "Delete" }));
    const dialogs = within(document.body).getAllByRole("dialog");
    const confirmDialog = dialogs[dialogs.length - 1];
    await user.click(within(confirmDialog).getByRole("button", { name: "Cancel" }));
    expect(deleteMutation.mutate).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    // After cancel the confirm dialog closes; only the outer edit dialog remains.
    expect(within(document.body).getAllByRole("dialog")).toHaveLength(1);
  });

  test("locked field shows Operator-edited badge; unlocked field does not", () => {
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    // DisplayName is locked → "Operator-edited" badge is present (rendered as a div by Badge).
    expect(
      within(document.body).getByText("Operator-edited", { selector: "div" }),
    ).toBeInTheDocument();
    // The locked input carries the ring accent class.
    const displayNameInput = screen.getByLabelText("Display name");
    expect(displayNameInput).toHaveClass("ring-1");
    // City is not locked → no accent ring.
    const cityInput = screen.getByLabelText("City");
    expect(cityInput).not.toHaveClass("ring-1");
  });

  test("Revert to auto is disabled while a PATCH is already in flight", () => {
    // Simulate a pending PATCH: the mutate call fires once but never resolves,
    // so the button should be rendered `disabled` — protecting against a
    // double-submit when the operator clicks twice in quick succession.
    patchMutation.isPending = true;
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    const revert = screen.getByRole("button", { name: "Revert to auto" });
    expect(revert).toBeDisabled();
  });

  test("dialog primitive smoke: renders role=dialog with the expected title", () => {
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    const dialog = screen.getByRole("dialog");
    expect(dialog).toBeInTheDocument();
    expect(screen.getByText("Edit catalogue entry")).toBeInTheDocument();
  });
});
