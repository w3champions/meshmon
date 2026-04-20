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
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { Campaign, PreviewDispatchResponse } from "@/api/hooks/campaigns";
import type { CatalogueEntry, CatalogueListResponse } from "@/api/hooks/catalogue";

// ---------------------------------------------------------------------------
// Module mocks. Register BEFORE importing the component under test so the
// real hooks never resolve.
// ---------------------------------------------------------------------------

const navigate = vi.fn();
vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return { ...actual, useNavigate: () => navigate };
});

vi.mock("@/api/hooks/agents", () => ({ useAgents: vi.fn() }));
vi.mock("@/api/hooks/campaign-stream", () => ({ useCampaignStream: vi.fn() }));
vi.mock("@/api/hooks/campaigns", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/campaigns")>("@/api/hooks/campaigns");
  return {
    ...actual,
    useCreateCampaign: vi.fn(),
    useStartCampaign: vi.fn(),
    useDeleteCampaign: vi.fn(),
    usePreviewDispatchCount: vi.fn(),
  };
});
vi.mock("@/api/hooks/catalogue", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/catalogue")>("@/api/hooks/catalogue");
  return {
    ...actual,
    useCatalogueListInfinite: vi.fn(),
    useCatalogueFacets: vi.fn(),
    useCatalogueMap: vi.fn(),
  };
});

// DrawMap pulls Leaflet which jsdom can't drive. Stub to a data-testid
// marker; the map-dialog behavior is covered by the DrawMap unit tests.
vi.mock("@/components/map/DrawMap", () => ({
  DrawMap: () => <div data-testid="draw-map-stub" />,
}));

// PasteStaging pulls Leaflet transitively; short-circuit it too.
vi.mock("@/components/catalogue/PasteStaging", () => ({
  PasteStaging: ({ open }: { open: boolean }) =>
    open ? <div data-testid="paste-staging-mock" /> : null,
}));

// Sonner-backed toast store — spy on `pushToast` so the error paths are
// verifiable without rendering the actual Sonner shell.
const pushToast = vi.fn();
vi.mock("@/stores/toast", () => ({
  useToastStore: {
    getState: () => ({ pushToast }),
  },
}));

// ---------------------------------------------------------------------------
// Imports AFTER mocks so vi.fn() stubs are in place.
// ---------------------------------------------------------------------------

import { useAgents } from "@/api/hooks/agents";
import {
  useCreateCampaign,
  useDeleteCampaign,
  usePreviewDispatchCount,
  useStartCampaign,
} from "@/api/hooks/campaigns";
import {
  useCatalogueFacets,
  useCatalogueListInfinite,
  useCatalogueMap,
} from "@/api/hooks/catalogue";
import CampaignComposer from "@/pages/CampaignComposer";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const FUTURE = new Date(Date.now() + 60_000).toISOString();

function makeAgent(id: string, lastSeenAt: string = FUTURE): AgentSummary {
  return {
    id,
    display_name: `Agent ${id}`,
    ip: `10.0.0.${id.length}`,
    last_seen_at: lastSeenAt,
    registered_at: "2026-01-01T00:00:00Z",
    catalogue_coordinates: { latitude: 52.5, longitude: 13.4 },
  };
}

function makeEntry(id: string, ip: string): CatalogueEntry {
  return {
    id,
    ip,
    display_name: `Host ${id}`,
    city: "Amsterdam",
    country_code: "NL",
    country_name: "Netherlands",
    asn: 1,
    network_operator: "Net",
    enrichment_status: "enriched",
    operator_edited_fields: [],
    created_at: "2026-01-01T00:00:00Z",
    source: "operator",
  };
}

function makeCampaign(id: string, overrides: Partial<Campaign> = {}): Campaign {
  return {
    id,
    title: "New campaign",
    notes: "",
    state: "draft",
    protocol: "icmp",
    evaluation_mode: "optimization",
    force_measurement: false,
    loss_threshold_pct: 2,
    stddev_weight: 1,
    probe_count: 10,
    probe_count_detail: 250,
    probe_stagger_ms: 100,
    timeout_ms: 2000,
    created_at: "2026-04-20T00:00:00Z",
    created_by: "alice",
    started_at: null,
    stopped_at: null,
    completed_at: null,
    evaluated_at: null,
    pair_counts: [],
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// Hook-return shapers. Each helper hides the `as unknown as ...` cast the
// test only needs enough of the real shape to drive the component.
// ---------------------------------------------------------------------------

interface PageSpec {
  entries: CatalogueEntry[];
  total: number;
  next_cursor: string | null;
}

function makeListResponse(spec: PageSpec): CatalogueListResponse {
  return {
    entries: spec.entries,
    total: spec.total,
    next_cursor: spec.next_cursor,
  };
}

interface ListInfiniteStubOptions {
  pages: CatalogueListResponse[];
  hasNextPage?: boolean;
  fetchNextPage?: ReturnType<typeof vi.fn>;
}

function listInfiniteStub(opts: ListInfiniteStubOptions) {
  const hasNext = opts.hasNextPage ?? Boolean(opts.pages.at(-1)?.next_cursor);
  return {
    data: {
      pages: opts.pages,
      pageParams: opts.pages.map((_, i) => (i === 0 ? undefined : `cursor-${i - 1}`)),
    },
    isLoading: false,
    isError: false,
    hasNextPage: hasNext,
    isFetchingNextPage: false,
    fetchNextPage: opts.fetchNextPage ?? vi.fn(),
  } as unknown as ReturnType<typeof useCatalogueListInfinite>;
}

function createMutationStub() {
  return { mutate: vi.fn(), mutateAsync: vi.fn(), isPending: false };
}

// ---------------------------------------------------------------------------
// Router harness — wraps the composer in an Outlet-based router so
// `useNavigate` resolves (but the real navigate is spied via the module
// mock above). A placeholder `/campaigns/$id` route lets a typed-cast
// navigate call land without a "route not found" boundary.
// ---------------------------------------------------------------------------

function renderComposer(initialPath = "/campaigns/new") {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: 0 },
      mutations: { retry: false },
    },
  });
  const rootRoute = createRootRoute({ component: Outlet });
  const composerRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns/new",
    component: CampaignComposer,
  });
  const campaignsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns",
    component: () => null,
  });
  const detailRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns/$id",
    component: () => null,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([composerRoute, campaignsRoute, detailRoute]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
  });
  return render(
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

// ---------------------------------------------------------------------------
// Shared mutation stubs — reset per-test in beforeEach.
// ---------------------------------------------------------------------------

const createStub = createMutationStub();
const startStub = createMutationStub();
const deleteStub = createMutationStub();

interface SetupOptions {
  agents?: AgentSummary[];
  pages?: CatalogueListResponse[];
  preview?: PreviewDispatchResponse | undefined;
  createResponse?: Campaign;
  fetchNextPage?: ReturnType<typeof vi.fn>;
  hasNextPage?: boolean;
}

function setupHooks(opts: SetupOptions = {}) {
  const agents = opts.agents ?? [makeAgent("a1"), makeAgent("a2"), makeAgent("a3")];
  vi.mocked(useAgents).mockReturnValue({
    data: agents,
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useAgents>);

  const pages = opts.pages ?? [
    makeListResponse({ entries: [makeEntry("e1", "192.168.1.1")], total: 1, next_cursor: null }),
  ];
  vi.mocked(useCatalogueListInfinite).mockReturnValue(
    listInfiniteStub({
      pages,
      fetchNextPage: opts.fetchNextPage,
      hasNextPage: opts.hasNextPage,
    }),
  );

  vi.mocked(useCatalogueFacets).mockReturnValue({
    data: undefined,
  } as unknown as ReturnType<typeof useCatalogueFacets>);

  vi.mocked(useCatalogueMap).mockReturnValue({
    data: undefined,
  } as unknown as ReturnType<typeof useCatalogueMap>);

  vi.mocked(usePreviewDispatchCount).mockReturnValue({
    data: opts.preview,
    isLoading: opts.preview === undefined,
  } as unknown as ReturnType<typeof usePreviewDispatchCount>);

  vi.mocked(useCreateCampaign).mockReturnValue(
    createStub as unknown as ReturnType<typeof useCreateCampaign>,
  );
  vi.mocked(useStartCampaign).mockReturnValue(
    startStub as unknown as ReturnType<typeof useStartCampaign>,
  );
  vi.mocked(useDeleteCampaign).mockReturnValue(
    deleteStub as unknown as ReturnType<typeof useDeleteCampaign>,
  );

  // Default `create.mutate` success path: invoke `onSuccess` with a fresh
  // draft campaign so the composer advances to phase 2 automatically.
  const createResponse = opts.createResponse ?? makeCampaign("campaign-xyz");
  createStub.mutate.mockImplementation(
    (_body: unknown, handlers?: { onSuccess?: (c: Campaign) => void }) => {
      handlers?.onSuccess?.(createResponse);
    },
  );
  // Default `start.mutate` success path: invoke `onSuccess` to drive navigation.
  startStub.mutate.mockImplementation((_id: string, handlers?: { onSuccess?: () => void }) => {
    handlers?.onSuccess?.();
  });
}

beforeEach(() => {
  navigate.mockReset();
  pushToast.mockReset();
  createStub.mutate.mockReset();
  startStub.mutate.mockReset();
  deleteStub.mutate.mockReset();
  // Default `delete.mutate` — invoke `onSettled` so Back-after-create
  // proceeds through to navigation in tests.
  deleteStub.mutate.mockImplementation((_id: string, handlers?: { onSettled?: () => void }) => {
    handlers?.onSettled?.();
  });
  setupHooks();
});

afterEach(() => {
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function fillTitle(user: ReturnType<typeof userEvent.setup>, value: string) {
  const titleInput = await screen.findByLabelText(/^title$/i);
  await user.clear(titleInput);
  await user.type(titleInput, value);
}

async function selectAllSources(user: ReturnType<typeof userEvent.setup>) {
  const section = screen.getByRole("region", { name: /sources/i });
  const addAll = within(section).getAllByRole("button", { name: /^add all$/i })[0];
  await user.click(addAll);
}

async function clickAddAllDestinationsAllPages(user: ReturnType<typeof userEvent.setup>) {
  const btn = await screen.findByRole("button", {
    name: /add all destinations \(all pages\)/i,
  });
  await user.click(btn);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("CampaignComposer — happy path", () => {
  test("walks catalogue pages, then Start fires create+start with full selection", async () => {
    // Two pages of 50 rows each. The walk hook reports `hasNextPage=true`
    // after the first page, then the stubbed `fetchNextPage` resolves with
    // both pages so the composer aggregates all 100 IPs.
    const page1Entries = Array.from({ length: 50 }, (_, i) =>
      makeEntry(`p1-${i}`, `10.0.0.${i + 1}`),
    );
    const page2Entries = Array.from({ length: 50 }, (_, i) =>
      makeEntry(`p2-${i}`, `10.1.0.${i + 1}`),
    );
    const page1 = makeListResponse({
      entries: page1Entries,
      total: 100,
      next_cursor: "cursor-1",
    });
    const page2 = makeListResponse({
      entries: page2Entries,
      total: 100,
      next_cursor: null,
    });

    // The walk calls `fetchNextPage()` once; the returned snapshot must
    // carry BOTH pages so the aggregation step sees all 100 IPs.
    const fetchNextPage = vi.fn().mockResolvedValue({
      data: {
        pages: [page1, page2],
        pageParams: [undefined, "cursor-1"],
      },
      hasNextPage: false,
    });

    setupHooks({
      pages: [page1],
      fetchNextPage,
      hasNextPage: true,
      preview: { fresh: 100, reusable: 0, total: 100 } as PreviewDispatchResponse,
    });

    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Global sweep");
    await selectAllSources(user);
    await clickAddAllDestinationsAllPages(user);

    await waitFor(() => {
      expect(fetchNextPage).toHaveBeenCalled();
    });

    // The composer-level walk strip disappears once aggregation completes.
    await waitFor(() => {
      expect(screen.queryByText(/collecting \d+/i)).not.toBeInTheDocument();
    });

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => expect(createStub.mutate).toHaveBeenCalled());
    const body = createStub.mutate.mock.calls[0]?.[0];
    expect(body).toMatchObject({
      title: "Global sweep",
      source_agent_ids: ["a1", "a2", "a3"],
      force_measurement: false,
    });
    expect(body.destination_ips).toHaveLength(100);

    // After create, the preview resolves with fresh=100 (< 1000 threshold)
    // so the start mutation fires automatically with the returned id.
    await waitFor(() =>
      expect(startStub.mutate).toHaveBeenCalledWith("campaign-xyz", expect.any(Object)),
    );
  });
});

describe("CampaignComposer — MTR disables Start", () => {
  test("protocol=mtr keeps the Start button disabled", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "mtr demo");
    const mtrToggle = await screen.findByRole("radio", { name: /mtr/i });
    await user.click(mtrToggle);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    expect(startButton).toBeDisabled();

    await user.click(startButton);
    expect(createStub.mutate).not.toHaveBeenCalled();
  });
});

describe("CampaignComposer — force_measurement in body", () => {
  test("toggling force_measurement on is echoed in the mutation payload", async () => {
    setupHooks({
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Forced");
    await selectAllSources(user);

    // Snapshot currently-loaded destinations via the composer's
    // exhaustive action — default setup fixture has a single IP.
    await clickAddAllDestinationsAllPages(user);

    const forceToggle = await screen.findByRole("button", { name: /force measurement/i });
    await user.click(forceToggle);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => expect(createStub.mutate).toHaveBeenCalled());
    const body = createStub.mutate.mock.calls[0]?.[0];
    expect(body).toMatchObject({ force_measurement: true });
  });
});

describe("CampaignComposer — threshold triggers confirm dialog", () => {
  test("fresh > 1000 opens the dialog; Cancel does not call start; Start does", async () => {
    setupHooks({
      preview: { fresh: 1500, reusable: 0, total: 1500 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Huge");
    await selectAllSources(user);
    await clickAddAllDestinationsAllPages(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    // Create fires and the auto-start effect reads preview.fresh=1500 →
    // opens the confirm dialog instead of auto-starting.
    const dialog = await screen.findByRole("dialog", { name: /confirm large dispatch/i });
    expect(dialog).toBeInTheDocument();
    expect(startStub.mutate).not.toHaveBeenCalled();

    await user.click(within(dialog).getByRole("button", { name: /cancel/i }));
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: /confirm large dispatch/i }),
      ).not.toBeInTheDocument(),
    );
    expect(startStub.mutate).not.toHaveBeenCalled();

    // Re-opening the dialog happens via the Start button — at this point
    // `draftCampaignId` is set, so the click routes through the phase 2
    // branch and re-opens the dialog.
    await user.click(startButton);
    const dialog2 = await screen.findByRole("dialog", { name: /confirm large dispatch/i });
    await user.click(within(dialog2).getByRole("button", { name: /^start$/i }));
    await waitFor(() =>
      expect(startStub.mutate).toHaveBeenCalledWith("campaign-xyz", expect.any(Object)),
    );
  });
});

describe("CampaignComposer — destination walk error", () => {
  test("fetchNextPage rejection surfaces toast + resets walk strip, dest set unchanged", async () => {
    const page1 = makeListResponse({
      entries: [makeEntry("x", "10.0.0.1")],
      total: 100,
      next_cursor: "cursor-1",
    });
    const fetchNextPage = vi.fn().mockRejectedValueOnce(new Error("boom"));

    setupHooks({ pages: [page1], fetchNextPage, hasNextPage: true });

    const user = userEvent.setup();
    renderComposer();

    await clickAddAllDestinationsAllPages(user);

    await waitFor(() => {
      expect(pushToast).toHaveBeenCalledWith(expect.objectContaining({ kind: "error" }));
    });

    // Walk strip clears.
    await waitFor(() => {
      expect(screen.queryByText(/collecting \d+/i)).not.toBeInTheDocument();
    });

    // The composer never advanced to Start — create.mutate is quiet.
    expect(createStub.mutate).not.toHaveBeenCalled();
  });
});

describe("CampaignComposer — navigate on start success", () => {
  test("navigate is called with /campaigns/$id when the start mutation resolves", async () => {
    setupHooks({
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
      createResponse: makeCampaign("done-id"),
    });

    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Nav Test");
    await selectAllSources(user);
    await clickAddAllDestinationsAllPages(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => {
      expect(navigate).toHaveBeenCalledWith(
        expect.objectContaining({
          to: "/campaigns/$id",
          params: { id: "done-id" },
        }),
      );
    });
  });
});

describe("CampaignComposer — offline sources", () => {
  test("offline agents are included in the create payload (backend skips per spec)", async () => {
    // Plan F.7 §623: operators can still pick offline agents; the backend
    // skips their pairs after 3 failed attempts. `a2` is driven stale by
    // nudging `last_seen_at` 10 minutes into the past (the `SourcePanel`
    // offline threshold is 5 min).
    const STALE = new Date(Date.now() - 10 * 60_000).toISOString();
    setupHooks({
      agents: [makeAgent("a1"), makeAgent("a2", STALE), makeAgent("a3")],
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
    });

    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Offline-inclusive");
    await selectAllSources(user);
    await clickAddAllDestinationsAllPages(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => expect(createStub.mutate).toHaveBeenCalled());
    const body = createStub.mutate.mock.calls[0]?.[0];
    expect(body.source_agent_ids).toEqual(expect.arrayContaining(["a1", "a2", "a3"]));
    expect(body.source_agent_ids).toContain("a2");
  });
});

describe("CampaignComposer — client-side validation gate", () => {
  test("rejects Start with empty title and toasts without calling create", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    // Wait for first render so `selectAllSources` finds the mounted panel.
    await screen.findByLabelText(/^title$/i);

    // Skip fillTitle — keep title empty. Sources + destinations must be
    // populated so the title check is the one that fires.
    await selectAllSources(user);
    await clickAddAllDestinationsAllPages(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    expect(pushToast).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "error", message: expect.stringMatching(/title/i) }),
    );
    expect(createStub.mutate).not.toHaveBeenCalled();
  });

  test("rejects Start with empty source set and toasts without calling create", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "No sources");
    // Skip selectAllSources. Populate destinations so the source gate fires.
    await clickAddAllDestinationsAllPages(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    expect(pushToast).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "error", message: expect.stringMatching(/source/i) }),
    );
    expect(createStub.mutate).not.toHaveBeenCalled();
  });

  test("rejects Start with empty destination set and toasts without calling create", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "No dests");
    await selectAllSources(user);
    // Skip clickAddAllDestinationsAllPages so destSet stays empty.

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    expect(pushToast).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "error", message: expect.stringMatching(/destination/i) }),
    );
    expect(createStub.mutate).not.toHaveBeenCalled();
  });
});

describe("CampaignComposer — draft lock after create", () => {
  test("draft-created banner renders and Back deletes the draft before navigating", async () => {
    // Large fresh count so the create resolves into the confirm dialog
    // (draftCampaignId set, start not yet fired) — this is the one
    // window in which Back-with-delete must fire.
    setupHooks({
      preview: { fresh: 1500, reusable: 0, total: 1500 } as PreviewDispatchResponse,
      createResponse: makeCampaign("draft-xyz"),
    });

    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Soon-to-be-abandoned");
    await selectAllSources(user);
    await clickAddAllDestinationsAllPages(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    // Confirm dialog opens because fresh > threshold, keeping the draft
    // in the limbo state the fix guards against.
    await screen.findByRole("dialog", { name: /confirm large dispatch/i });
    expect(await screen.findByText(/draft created — further edits/i)).toBeInTheDocument();

    // The user cancels out of the confirm dialog so we're sitting on the
    // composer with `draftCampaignId` set — the exact state Back must
    // clean up.
    await user.click(screen.getByRole("button", { name: /cancel/i }));

    // Now click Back. It must delete the draft before navigating.
    const backButton = screen.getByRole("button", { name: /^back$/i });
    await user.click(backButton);

    await waitFor(() =>
      expect(deleteStub.mutate).toHaveBeenCalledWith("draft-xyz", expect.any(Object)),
    );
    await waitFor(() =>
      expect(navigate).toHaveBeenCalledWith(expect.objectContaining({ to: "/campaigns" })),
    );
  });

  test("Back before create fires navigation without touching delete", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    await screen.findByLabelText(/^title$/i);
    const backButton = screen.getByRole("button", { name: /^back$/i });
    await user.click(backButton);

    expect(deleteStub.mutate).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(navigate).toHaveBeenCalledWith(expect.objectContaining({ to: "/campaigns" })),
    );
  });

  test("knob inputs are disabled once the draft exists", async () => {
    // Force the draft into limbo (confirm dialog open, not yet started).
    setupHooks({
      preview: { fresh: 1500, reusable: 0, total: 1500 } as PreviewDispatchResponse,
      createResponse: makeCampaign("locked-id"),
    });

    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Locked");
    await selectAllSources(user);
    await clickAddAllDestinationsAllPages(user);

    await user.click(screen.getByRole("button", { name: /^start(ing…)?$/i }));
    await screen.findByRole("dialog", { name: /confirm large dispatch/i });
    // Close the dialog so the disabled-state is visible without the
    // portal overlay blocking pointer events.
    await user.click(screen.getByRole("button", { name: /cancel/i }));

    // Title input is disabled — attempting to type must be a no-op.
    const titleInput = screen.getByLabelText(/^title$/i) as HTMLInputElement;
    expect(titleInput).toBeDisabled();
  });
});

describe("CampaignComposer — exhaustive walk guards", () => {
  test("button is disabled while the catalogue is still loading", async () => {
    // `useCatalogueListInfinite` mocked to a pre-first-page state: no
    // pages, `isLoading=true`. The exhaustive-walk button must be
    // disabled so the empty-snapshot race can't fire.
    vi.mocked(useCatalogueListInfinite).mockReturnValue({
      data: undefined,
      isLoading: true,
      isError: false,
      isFetching: true,
      hasNextPage: false,
      fetchNextPage: vi.fn(),
    } as unknown as ReturnType<typeof useCatalogueListInfinite>);

    const user = userEvent.setup();
    renderComposer();

    const btn = await screen.findByRole("button", {
      name: /add all destinations \(all pages\)/i,
    });
    expect(btn).toBeDisabled();
    await user.click(btn);

    // Nothing happens — destSet untouched, no toast, no fetchNextPage call.
    expect(pushToast).not.toHaveBeenCalled();
  });

  test("empty-catalogue snapshot guard toasts and leaves destSet untouched", async () => {
    // This path is reached if the click somehow lands with an empty
    // `pages` array even though the button is enabled — e.g. a filter
    // change flips `isFetching` false between render and click. The
    // guard is defence-in-depth beyond the button-disabled gate.
    const fetchNextPage = vi.fn();
    vi.mocked(useCatalogueListInfinite).mockReturnValue({
      data: { pages: [], pageParams: [] },
      isLoading: false,
      isError: false,
      isFetching: false,
      hasNextPage: false,
      fetchNextPage,
    } as unknown as ReturnType<typeof useCatalogueListInfinite>);

    const user = userEvent.setup();
    renderComposer();

    const btn = await screen.findByRole("button", {
      name: /add all destinations \(all pages\)/i,
    });
    await user.click(btn);

    expect(pushToast).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "error",
        message: expect.stringMatching(/catalogue still loading/i),
      }),
    );
    expect(fetchNextPage).not.toHaveBeenCalled();
  });
});
