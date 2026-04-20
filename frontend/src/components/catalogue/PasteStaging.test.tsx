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
