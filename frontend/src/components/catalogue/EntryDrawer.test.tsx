import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { type ReactNode, useEffect } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import {
  useDeleteCatalogueEntry,
  usePatchCatalogueEntry,
  useReenrichOne,
} from "@/api/hooks/catalogue";
import { EntryDrawer } from "@/components/catalogue/EntryDrawer";
import { IpHostnameProvider } from "@/components/ip-hostname";
import { useIpHostnameContext } from "@/components/ip-hostname/IpHostnameProvider";

vi.mock("@/api/hooks/catalogue", () => ({
  usePatchCatalogueEntry: vi.fn(),
  useReenrichOne: vi.fn(),
  useDeleteCatalogueEntry: vi.fn(),
}));

// Stub the Leaflet-backed picker — jsdom can't render the real map and
// these tests only care about the prop wiring. The two buttons drive
// the onChange handler the drawer passes in.
vi.mock("@/components/map/LocationPicker", () => ({
  LocationPicker: ({
    value,
    onChange,
  }: {
    value: { latitude: number; longitude: number } | null;
    onChange(next: { latitude: number; longitude: number } | null): void;
  }) => (
    <div data-testid="location-picker-stub">
      <button
        type="button"
        aria-label="test pick location"
        onClick={() => onChange({ latitude: 1.5, longitude: 2.5 })}
      >
        Pick
      </button>
      <button type="button" aria-label="test clear location" onClick={() => onChange(null)}>
        Clear
      </button>
      <span data-testid="location-picker-value">
        {value ? `${value.latitude},${value.longitude}` : "null"}
      </span>
    </div>
  ),
}));

type Mutation = {
  mutate: ReturnType<typeof vi.fn>;
  isPending: boolean;
};

function makeMutation(): Mutation {
  return { mutate: vi.fn(), isPending: false };
}

/**
 * Minimal EventSource stub. The `IpHostnameProvider` opens the SSE stream on
 * mount; jsdom has no native implementation. These tests don't exercise the
 * stream (the refresh button posts; the SSE event delivery is provider-local
 * and already covered by `IpHostnameProvider.test.tsx`).
 */
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

/**
 * Mount-only seed helper. Primes the provider map before first paint so the
 * hostname row renders `ip (hostname)` instead of a bare IP.
 */
function Seeder({
  seed,
  children,
}: {
  seed: Array<{ ip: string; hostname?: string | null }>;
  children: ReactNode;
}) {
  const { seedFromResponse } = useIpHostnameContext();
  // biome-ignore lint/correctness/useExhaustiveDependencies: mount-only seed
  useEffect(() => {
    if (seed.length > 0) seedFromResponse(seed);
  }, []);
  return <>{children}</>;
}

function wrap(seed: Array<{ ip: string; hostname?: string | null }> = []) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>
      <IpHostnameProvider>
        <Seeder seed={seed}>{children}</Seeder>
      </IpHostnameProvider>
    </QueryClientProvider>
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
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
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
    // Location no longer renders raw number inputs — the picker stub
    // surfaces the current coordinates in a status readout.
    expect(screen.queryByLabelText("Latitude")).toBeNull();
    expect(screen.queryByLabelText("Longitude")).toBeNull();
    expect(screen.getByTestId("location-picker-value").textContent).toBe("37.77,-122.42");
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

  test("picking a new location dirties both latitude and longitude and PATCHes both", async () => {
    const user = userEvent.setup();
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    await user.click(screen.getByRole("button", { name: /test pick location/i }));
    await user.click(screen.getByRole("button", { name: "Save" }));

    expect(patchMutation.mutate).toHaveBeenCalledTimes(1);
    const [variables] = patchMutation.mutate.mock.calls[0];
    expect(variables.id).toBe(ENTRY.id);
    // Both halves land on the wire, atomically.
    expect(variables.patch).toEqual({ latitude: 1.5, longitude: 2.5 });
  });

  test("Location row shows Operator-edited when either half is locked", () => {
    // Pre-lock only Latitude — Longitude is still unlocked, but the
    // paired semantics mean the row must read as operator-edited.
    render(
      <EntryDrawer entry={{ ...ENTRY, operator_edited_fields: ["Latitude"] }} onClose={vi.fn()} />,
      { wrapper: wrap() },
    );
    const reverts = screen.getAllByRole("button", { name: "Revert to auto" });
    // One button for the Latitude-only lock (on the composite Location row).
    expect(reverts.length).toBeGreaterThanOrEqual(1);
  });

  test("Reverting Location PATCHes both Latitude and Longitude at once", async () => {
    const user = userEvent.setup();
    render(
      <EntryDrawer
        entry={{ ...ENTRY, operator_edited_fields: ["Latitude", "Longitude"] }}
        onClose={vi.fn()}
      />,
      { wrapper: wrap() },
    );
    // The locked Location row is the only row that renders a Revert
    // button in this entry (the other fields are unlocked).
    const revert = screen.getByRole("button", { name: "Revert to auto" });
    await user.click(revert);

    expect(patchMutation.mutate).toHaveBeenCalledTimes(1);
    const [variables] = patchMutation.mutate.mock.calls[0];
    expect(variables.patch).toEqual({
      revert_to_auto: ["Latitude", "Longitude"],
      latitude: null,
      longitude: null,
    });
  });

  test("dialog primitive smoke: renders role=dialog with the expected title", () => {
    render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });
    const dialog = screen.getByRole("dialog");
    expect(dialog).toBeInTheDocument();
    expect(screen.getByText("Edit catalogue entry")).toBeInTheDocument();
  });

  // -----------------------------------------------------------------------
  // Hostname refresh button
  // -----------------------------------------------------------------------

  describe("hostname refresh", () => {
    test("renders the resolved hostname from the provider seed", () => {
      render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, {
        wrapper: wrap([{ ip: ENTRY.ip, hostname: "alpha.example.com" }]),
      });
      expect(screen.getByText("(alpha.example.com)")).toBeInTheDocument();
    });

    test("clicking Refresh hostname POSTs to /api/hostnames/:ip/refresh and disables optimistically", async () => {
      const fetchSpy = vi
        .spyOn(globalThis, "fetch")
        .mockResolvedValue(new Response(null, { status: 202 }));

      const user = userEvent.setup();
      render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });

      const button = screen.getByRole("button", { name: /refresh hostname/i });
      expect(button).not.toBeDisabled();

      await user.click(button);

      // Optimistic disable — the handler fires `setPending(true)` before
      // awaiting the POST.
      expect(button).toBeDisabled();
      expect(fetchSpy).toHaveBeenCalledTimes(1);
      const [url, init] = fetchSpy.mock.calls[0] ?? [];
      expect(url).toBe(`/api/hostnames/${ENTRY.ip}/refresh`);
      expect(init?.method).toBe("POST");

      // After the 2 s cooldown the button re-enables. waitFor defaults to a
      // 1 s interval cap on retries, well inside the 5 s Vitest test budget
      // once the real timer fires.
      await waitFor(
        () => {
          expect(button).not.toBeDisabled();
        },
        { timeout: 3000 },
      );
    });

    test("failed refresh toasts and the button re-enables after the cooldown", async () => {
      const { toast } = await import("sonner");
      const errorSpy = vi.spyOn(toast, "error").mockReturnValue("toast-id");
      vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(null, { status: 429 }));

      const user = userEvent.setup();
      render(<EntryDrawer entry={ENTRY} onClose={vi.fn()} />, { wrapper: wrap() });

      const button = screen.getByRole("button", { name: /refresh hostname/i });
      await user.click(button);

      // The rejected fetch resolves the handler's catch block — wait until
      // toast.error has been called so the assertion is deterministic.
      await waitFor(() => {
        expect(errorSpy).toHaveBeenCalled();
      });
      const firstArg = errorSpy.mock.calls[0]?.[0];
      expect(String(firstArg)).toMatch(/refresh hostname|HTTP 429/i);

      // Button stays disabled until the cooldown elapses, even on failure.
      expect(button).toBeDisabled();
      await waitFor(
        () => {
          expect(button).not.toBeDisabled();
        },
        { timeout: 3000 },
      );
    });
  });
});
