import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { beforeEach, describe, expect, test, vi } from "vitest";
import type { CatalogueEntry, CataloguePasteResponse } from "@/api/hooks/catalogue";
import { catalogueEntryKey, usePasteCatalogue } from "@/api/hooks/catalogue";
import { PasteStaging } from "@/components/catalogue/PasteStaging";

vi.mock("@/api/hooks/catalogue", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/hooks/catalogue")>();
  return {
    ...actual,
    usePasteCatalogue: vi.fn(),
  };
});

// Stub the Leaflet-backed picker — jsdom cannot render real Leaflet
// and the bulk-metadata tests only care about the prop wiring. The
// button exposes both a "pick" and a "clear" surface; the rendered
// coordinates make the effect visible in the DOM.
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
        onClick={() => onChange({ latitude: 37.7749, longitude: -122.4194 })}
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

// Stub the country picker so tests can emit {code, name} atomically
// without driving the real Radix Select under jsdom.
vi.mock("@/components/catalogue/CountryPicker", () => ({
  CountryPicker: ({
    value,
    onChange,
  }: {
    value: { code: string; name: string } | null;
    onChange(next: { code: string; name: string } | null): void;
  }) => (
    <div data-testid="country-picker-stub">
      <button
        type="button"
        aria-label="test pick country"
        onClick={() => onChange({ code: "US", name: "United States" })}
      >
        Pick
      </button>
      <span data-testid="country-picker-value">
        {value ? `${value.code}:${value.name}` : "null"}
      </span>
    </div>
  ),
}));

type MutationShape = {
  mutate: ReturnType<typeof vi.fn>;
  mutateAsync: ReturnType<typeof vi.fn>;
  isPending: boolean;
  isError: boolean;
};

function makeMutation(overrides: Partial<MutationShape> = {}): MutationShape {
  return {
    mutate: vi.fn(),
    mutateAsync: vi.fn(),
    isPending: false,
    isError: false,
    ...overrides,
  };
}

let queryClient: QueryClient;

function wrap() {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  );
}

const ENTRY: CatalogueEntry = {
  id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
  ip: "1.2.3.4",
  created_at: "2026-04-20T10:00:00Z",
  created_by: "operator",
  enriched_at: null,
  source: "operator",
  enrichment_status: "pending",
  operator_edited_fields: [],
  display_name: null,
  asn: null,
  country_code: null,
  country_name: null,
  city: null,
  latitude: null,
  longitude: null,
  network_operator: null,
  website: null,
  notes: null,
};

beforeEach(() => {
  queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.mocked(usePasteCatalogue).mockReturnValue(
    makeMutation() as unknown as ReturnType<typeof usePasteCatalogue>,
  );
});

describe("PasteStaging", () => {
  test("typing IPs and clicking Add calls usePasteCatalogue with the right body", async () => {
    const user = userEvent.setup();
    const mutateAsync = vi.fn().mockResolvedValue({
      created: [ENTRY],
      existing: [],
      invalid: [],
    } satisfies CataloguePasteResponse);
    vi.mocked(usePasteCatalogue).mockReturnValue(
      makeMutation({ mutateAsync }) as unknown as ReturnType<typeof usePasteCatalogue>,
    );

    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });
    const textarea = within(document.body).getByRole("textbox", { name: /paste ip/i });
    await user.type(textarea, "1.2.3.4\n5.6.7.8");
    await user.click(within(document.body).getByRole("button", { name: /add/i }));

    expect(mutateAsync).toHaveBeenCalledTimes(1);
    const body = mutateAsync.mock.calls[0][0];
    expect(body).toEqual({ ips: expect.arrayContaining(["1.2.3.4", "5.6.7.8"]) });
    expect(body.ips).toHaveLength(2);
  });

  test("staging table renders one row per accepted IP with a pending chip", async () => {
    const user = userEvent.setup();
    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });
    const textarea = within(document.body).getByRole("textbox", { name: /paste ip/i });
    await user.type(textarea, "10.0.0.1\n10.0.0.2");

    // Two rows in the parsed IPs table (rows[0] is header, rows[1..] are data)
    const rows = within(document.body).getAllByRole("row");
    expect(rows.length).toBeGreaterThanOrEqual(3);
    expect(within(document.body).getByText("10.0.0.1")).toBeInTheDocument();
    expect(within(document.body).getByText("10.0.0.2")).toBeInTheDocument();
    // Each accepted row shows a pending chip
    const pendingChips = within(document.body).getAllByText("Pending");
    expect(pendingChips.length).toBeGreaterThanOrEqual(2);
  });

  test("invalid tokens render as red chips with the parse error on hover", async () => {
    const user = userEvent.setup();
    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });
    const textarea = within(document.body).getByRole("textbox", { name: /paste ip/i });
    await user.type(textarea, "not-an-ip");

    // The rejected token renders as a chip inside the invalid-tokens list
    const invalidList = within(document.body).getByRole("list", { name: /invalid tokens/i });
    const chip = within(invalidList).getByText("not-an-ip");
    // Hover tooltip surfaces the parse error via the `title` attribute
    expect(chip).toHaveAttribute("title", "Not a valid IP address");
  });

  test("intra-paste duplicates collapse to one row with ×N badge", async () => {
    const user = userEvent.setup();
    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });
    const textarea = within(document.body).getByRole("textbox", { name: /paste ip/i });
    await user.type(textarea, "1.2.3.4\n1.2.3.4\n1.2.3.4");

    // Only one row for that IP
    const ipCells = within(document.body).getAllByText("1.2.3.4");
    expect(ipCells).toHaveLength(1);
    // Badge showing ×3
    expect(within(document.body).getByText("×3")).toBeInTheDocument();
  });

  test("typing a /24 CIDR shows the exact inline error copy on hover", async () => {
    const user = userEvent.setup();
    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });
    const textarea = within(document.body).getByRole("textbox", { name: /paste ip/i });
    await user.type(textarea, "192.168.1.0/24");

    // The rejected CIDR renders as a red chip inside the invalid-tokens list;
    // the exact error copy lives in the `title` attribute (hover tooltip).
    const invalidList = within(document.body).getByRole("list", { name: /invalid tokens/i });
    const chip = within(invalidList).getByText("192.168.1.0/24");
    expect(chip).toHaveAttribute(
      "title",
      "IP addresses only — CIDR ranges aren't allowed as catalogue entries",
    );
  });

  test("dialog primitive smoke: renders role=dialog with Add IPs title", () => {
    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });
    const dialog = within(document.body).getByRole("dialog");
    expect(dialog).toBeInTheDocument();
    expect(within(document.body).getByText("Add IPs")).toBeInTheDocument();
  });

  test("metadata panel is collapsed by default and expands on click", async () => {
    const user = userEvent.setup();
    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });

    // Collapsed: the toggle exists but the inner fields are hidden.
    const toggle = within(document.body).getByRole("button", {
      name: /default metadata/i,
    });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(within(document.body).queryByTestId("location-picker-stub")).toBeNull();

    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(within(document.body).getByTestId("location-picker-stub")).toBeInTheDocument();
    expect(within(document.body).getByTestId("country-picker-stub")).toBeInTheDocument();
  });

  test("Add with no metadata omits the metadata key from the wire body", async () => {
    const user = userEvent.setup();
    const mutateAsync = vi.fn().mockResolvedValue({
      created: [ENTRY],
      existing: [],
      invalid: [],
    } satisfies CataloguePasteResponse);
    vi.mocked(usePasteCatalogue).mockReturnValue(
      makeMutation({ mutateAsync }) as unknown as ReturnType<typeof usePasteCatalogue>,
    );

    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });
    const textarea = within(document.body).getByRole("textbox", { name: /paste ip/i });
    await user.type(textarea, "1.2.3.4");
    await user.click(within(document.body).getByRole("button", { name: /^add$/i }));

    const body = mutateAsync.mock.calls[0][0];
    expect(body).not.toHaveProperty("metadata");
  });

  test("filled metadata + Add sends metadata in the wire body", async () => {
    const user = userEvent.setup();
    const mutateAsync = vi.fn().mockResolvedValue({
      created: [ENTRY],
      existing: [],
      invalid: [],
      skipped_summary: { rows_with_skips: 0, skipped_field_counts: {} },
    } satisfies CataloguePasteResponse);
    vi.mocked(usePasteCatalogue).mockReturnValue(
      makeMutation({ mutateAsync }) as unknown as ReturnType<typeof usePasteCatalogue>,
    );

    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });

    const textarea = within(document.body).getByRole("textbox", { name: /paste ip/i });
    await user.type(textarea, "1.2.3.4");

    // Expand panel and fill fields.
    await user.click(within(document.body).getByRole("button", { name: /default metadata/i }));
    await user.type(
      within(document.body).getByRole("textbox", { name: /display name/i }),
      "fastly-sfo",
    );
    await user.type(within(document.body).getByRole("textbox", { name: /^city$/i }), "SF");
    await user.click(within(document.body).getByRole("button", { name: /test pick country/i }));
    await user.click(within(document.body).getByRole("button", { name: /test pick location/i }));
    await user.type(
      within(document.body).getByRole("textbox", { name: /website/i }),
      "https://example.com",
    );
    await user.type(within(document.body).getByRole("textbox", { name: /^notes$/i }), "seeded");

    await user.click(within(document.body).getByRole("button", { name: /^add$/i }));

    const body = mutateAsync.mock.calls[0][0];
    expect(body.ips).toEqual(["1.2.3.4"]);
    expect(body.metadata).toEqual({
      display_name: "fastly-sfo",
      city: "SF",
      country_code: "US",
      country_name: "United States",
      latitude: 37.7749,
      longitude: -122.4194,
      website: "https://example.com",
      notes: "seeded",
    });
  });

  test("surfaces a skipped notice when skipped_summary has rows_with_skips", async () => {
    const user = userEvent.setup();
    const mutateAsync = vi.fn().mockResolvedValue({
      created: [],
      existing: [
        { ...ENTRY, id: "cccccccc-cccc-cccc-cccc-cccccccccccc", ip: "1.2.3.4" },
        { ...ENTRY, id: "dddddddd-dddd-dddd-dddd-dddddddddddd", ip: "5.6.7.8" },
      ],
      invalid: [],
      skipped_summary: {
        rows_with_skips: 2,
        skipped_field_counts: { Location: 2 },
      },
    } satisfies CataloguePasteResponse);
    vi.mocked(usePasteCatalogue).mockReturnValue(
      makeMutation({ mutateAsync }) as unknown as ReturnType<typeof usePasteCatalogue>,
    );

    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });
    const textarea = within(document.body).getByRole("textbox", { name: /paste ip/i });
    await user.type(textarea, "1.2.3.4\n5.6.7.8");
    // Expand + pick location so `metadata` is sent.
    await user.click(within(document.body).getByRole("button", { name: /default metadata/i }));
    await user.click(within(document.body).getByRole("button", { name: /test pick location/i }));
    await user.click(within(document.body).getByRole("button", { name: /^add$/i }));

    // Notice carries the row count and the skipped field label.
    await waitFor(() => {
      const notice = within(document.body).getByRole("status", {
        name: /metadata skip summary/i,
      });
      expect(notice.textContent).toMatch(/2/);
      expect(notice.textContent).toMatch(/Location/);
    });
  });

  test("chip flips to enriched when SSE enrichment_progress updates the query cache", async () => {
    const user = userEvent.setup();
    const mutateAsync = vi.fn().mockResolvedValue({
      created: [{ ...ENTRY, id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", ip: "1.2.3.4" }],
      existing: [],
      invalid: [],
    } satisfies CataloguePasteResponse);
    vi.mocked(usePasteCatalogue).mockReturnValue(
      makeMutation({ mutateAsync }) as unknown as ReturnType<typeof usePasteCatalogue>,
    );

    render(<PasteStaging open={true} onOpenChange={vi.fn()} />, { wrapper: wrap() });
    const textarea = within(document.body).getByRole("textbox", { name: /paste ip/i });
    await user.type(textarea, "1.2.3.4");
    await user.click(within(document.body).getByRole("button", { name: /add/i }));

    // After POST success the row is keyed by id from the response
    await waitFor(() => {
      // The chip should still exist (panel stays mounted after POST)
      expect(within(document.body).getAllByText("1.2.3.4").length).toBeGreaterThanOrEqual(1);
    });

    // Simulate SSE enrichment_progress: directly set the query cache
    const entryId = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    queryClient.setQueryData<CatalogueEntry>(catalogueEntryKey(entryId), {
      ...ENTRY,
      id: entryId,
      ip: "1.2.3.4",
      enrichment_status: "enriched",
    });

    // The chip should flip to enriched
    await waitFor(() => {
      expect(within(document.body).getByText("Enriched")).toBeInTheDocument();
    });
  });
});
