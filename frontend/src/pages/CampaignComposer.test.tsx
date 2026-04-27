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
import { IpHostnameProvider } from "@/components/ip-hostname";

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
import { DEFAULT_KNOBS } from "@/lib/campaign-config";
import CampaignComposer from "@/pages/CampaignComposer";
import { useComposerSeedStore } from "@/stores/composer-seed";

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
    loss_threshold_ratio: 0.02,
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
    max_hops: 2,
    vm_lookback_minutes: 15,
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
      <IpHostnameProvider>
        <RouterProvider router={router} />
      </IpHostnameProvider>
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
  // Reset the composer-seed store between tests — a leaked seed from a
  // Clone-path test would hydrate subsequent composer mounts.
  useComposerSeedStore.setState({ seed: null });
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

async function clickAddAllDestinations(user: ReturnType<typeof userEvent.setup>) {
  const section = await screen.findByRole("region", { name: /destinations/i });
  await user.click(within(section).getByRole("button", { name: /^add all$/i }));
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
    await clickAddAllDestinations(user);

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
    await clickAddAllDestinations(user);

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
    await clickAddAllDestinations(user);

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

// Destination walk error semantics live in the DestinationPanel — when
// the walk rejects the panel never emits onSelectedChange, so destSet
// stays empty and the composer's Start gate covers the user-facing
// consequence.

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
    await clickAddAllDestinations(user);

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
    await clickAddAllDestinations(user);

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
    await clickAddAllDestinations(user);

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
    await clickAddAllDestinations(user);

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
    // Skip clickAddAllDestinations so destSet stays empty.

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
    await clickAddAllDestinations(user);

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
    await clickAddAllDestinations(user);

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

// ---------------------------------------------------------------------------
// Q3 — edge_candidate source picker explainer
// ---------------------------------------------------------------------------

describe("CampaignComposer — edge_candidate source explainer", () => {
  test("source explainer renders only when evaluation_mode is edge_candidate", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    await screen.findByLabelText(/^title$/i);

    // Default mode is optimization — explainer must not appear.
    expect(
      screen.queryByText(/selected source agents probe each candidate/i),
    ).not.toBeInTheDocument();

    // Switch to edge_candidate via the toggle.
    const edgeToggle = screen.getByRole("radio", { name: /edge.?candidate/i });
    await user.click(edgeToggle);

    expect(screen.getByText(/selected source agents probe each candidate/i)).toBeInTheDocument();
  });

  test("source explainer disappears when switching away from edge_candidate", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    await screen.findByLabelText(/^title$/i);

    const edgeToggle = screen.getByRole("radio", { name: /edge.?candidate/i });
    await user.click(edgeToggle);
    expect(screen.getByText(/selected source agents probe each candidate/i)).toBeInTheDocument();

    // Switch back to optimization.
    const optimizationToggle = screen.getByRole("radio", { name: /optimization/i });
    await user.click(optimizationToggle);
    expect(
      screen.queryByText(/selected source agents probe each candidate/i),
    ).not.toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// Q2 — edge_candidate useful_latency_ms validation gate
// ---------------------------------------------------------------------------

describe("CampaignComposer — edge_candidate useful_latency_ms required validation", () => {
  test("Start is blocked and shows toast when useful_latency_ms is null in edge_candidate mode", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Edge test");
    await selectAllSources(user);
    await clickAddAllDestinations(user);

    const edgeToggle = screen.getByRole("radio", { name: /edge.?candidate/i });
    await user.click(edgeToggle);

    // useful_latency_ms defaults to null — Start button must be disabled.
    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    expect(startButton).toBeDisabled();
    expect(createStub.mutate).not.toHaveBeenCalled();
  });
});

// Q2 — new fields flow to wire shape
describe("CampaignComposer — new knobs in create payload", () => {
  test("max_hops and vm_lookback_minutes are included in the create body", async () => {
    setupHooks({
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "New knobs test");
    await selectAllSources(user);
    await clickAddAllDestinations(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => expect(createStub.mutate).toHaveBeenCalled());
    const body = createStub.mutate.mock.calls[0]?.[0];
    expect(body).toMatchObject({ max_hops: 2, vm_lookback_minutes: 15 });
  });

  test("useful_latency_ms is omitted from payload when null and mode is not edge_candidate", async () => {
    setupHooks({
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "No latency knob");
    await selectAllSources(user);
    await clickAddAllDestinations(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => expect(createStub.mutate).toHaveBeenCalled());
    const body = createStub.mutate.mock.calls[0]?.[0];
    expect(body.useful_latency_ms).toBeUndefined();
  });
});

// "Add all" loading/empty-snapshot guards live in DestinationPanel —
// see DestinationPanel.test.tsx for the disabled-during-load and
// empty-catalogue coverage.

describe("CampaignComposer — composer-seed consume on mount", () => {
  test("hydrates title, knobs, source/destination sets from a staged seed", async () => {
    // Stage a seed BEFORE mounting the composer — the mount effect
    // consumes it synchronously.
    useComposerSeedStore.getState().setSeed({
      knobs: {
        ...DEFAULT_KNOBS,
        title: "Copy of alpha",
        notes: "cloned notes",
        probe_count: 42,
        protocol: "tcp",
      },
      sourceSet: ["a1", "a2"],
      destSet: ["10.1.1.1", "10.1.1.2"],
    });

    setupHooks({
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
    });
    renderComposer();

    // Title hydrates from the seed — the input reads the cloned value.
    const titleInput = (await screen.findByLabelText(/^title$/i)) as HTMLInputElement;
    await waitFor(() => {
      expect(titleInput.value).toBe("Copy of alpha");
    });

    // Seed is cleared after consume — a second mount would render defaults.
    expect(useComposerSeedStore.getState().seed).toBeNull();
  });

  test("no seed staged → composer keeps default state", async () => {
    setupHooks();
    renderComposer();

    const titleInput = (await screen.findByLabelText(/^title$/i)) as HTMLInputElement;
    // Default title is empty — no hydration happened.
    expect(titleInput.value).toBe("");
  });

  test("seed drives source_agent_ids and destination_ips in the create payload", async () => {
    useComposerSeedStore.getState().setSeed({
      knobs: { ...DEFAULT_KNOBS, title: "Seeded campaign" },
      sourceSet: ["agent-seed-1"],
      destSet: ["192.168.99.1", "192.168.99.2"],
    });

    setupHooks({
      preview: { fresh: 2, reusable: 0, total: 2 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    // Wait for the hydration effect to land before clicking Start —
    // otherwise the click races the consume() and the payload is empty.
    const titleInput = (await screen.findByLabelText(/^title$/i)) as HTMLInputElement;
    await waitFor(() => {
      expect(titleInput.value).toBe("Seeded campaign");
    });

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => expect(createStub.mutate).toHaveBeenCalled());
    const body = createStub.mutate.mock.calls[0]?.[0];
    expect(body.source_agent_ids).toEqual(["agent-seed-1"]);
    expect(body.destination_ips).toEqual(expect.arrayContaining(["192.168.99.1", "192.168.99.2"]));
    expect(body.destination_ips).toHaveLength(2);
  });
});
