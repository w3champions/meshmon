/**
 * Page-level integration tests for CampaignComposer in edge_candidate mode.
 * Mounts the full composer page with only the data-layer hooks mocked.
 * Mirrors the patterns from CampaignComposer.test.tsx.
 */
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
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { Campaign, PreviewDispatchResponse } from "@/api/hooks/campaigns";
import type { CatalogueEntry, CatalogueListResponse } from "@/api/hooks/catalogue";
import { IpHostnameProvider } from "@/components/ip-hostname";

// ---------------------------------------------------------------------------
// Module mocks — register BEFORE importing the component under test.
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

vi.mock("@/components/map/DrawMap", () => ({
  DrawMap: () => <div data-testid="draw-map-stub" />,
}));

vi.mock("@/components/catalogue/PasteStaging", () => ({
  PasteStaging: ({ open }: { open: boolean }) =>
    open ? <div data-testid="paste-staging-mock" /> : null,
}));

const pushToast = vi.fn();
vi.mock("@/stores/toast", () => ({
  useToastStore: {
    getState: () => ({ pushToast }),
  },
}));

// ---------------------------------------------------------------------------
// Imports AFTER mocks.
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

function makeAgent(id: string, lastSeenAt = FUTURE): AgentSummary {
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

function makeListResponse(
  entries: CatalogueEntry[],
  total: number,
  next_cursor: string | null,
): CatalogueListResponse {
  return { entries, total, next_cursor };
}

function listInfiniteStub(pages: CatalogueListResponse[]) {
  return {
    data: {
      pages,
      pageParams: pages.map((_, i) => (i === 0 ? undefined : `cursor-${i - 1}`)),
    },
    isLoading: false,
    isError: false,
    hasNextPage: Boolean(pages.at(-1)?.next_cursor),
    isFetchingNextPage: false,
    fetchNextPage: vi.fn(),
  } as unknown as ReturnType<typeof useCatalogueListInfinite>;
}

function createMutationStub() {
  return { mutate: vi.fn(), mutateAsync: vi.fn(), isPending: false };
}

// ---------------------------------------------------------------------------
// Router harness
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
}

function setupHooks(opts: SetupOptions = {}) {
  const agents = opts.agents ?? [makeAgent("a1"), makeAgent("a2")];
  vi.mocked(useAgents).mockReturnValue({
    data: agents,
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useAgents>);

  const pages = opts.pages ?? [makeListResponse([makeEntry("e1", "192.168.1.1")], 1, null)];
  vi.mocked(useCatalogueListInfinite).mockReturnValue(listInfiniteStub(pages));

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

  const createResponse = opts.createResponse ?? makeCampaign("campaign-xyz");
  createStub.mutate.mockImplementation(
    (_body: unknown, handlers?: { onSuccess?: (c: Campaign) => void }) => {
      handlers?.onSuccess?.(createResponse);
    },
  );
  startStub.mutate.mockImplementation((_id: string, handlers?: { onSuccess?: () => void }) => {
    handlers?.onSuccess?.();
  });
}

// ---------------------------------------------------------------------------
// Setup / teardown
// ---------------------------------------------------------------------------

beforeEach(() => {
  navigate.mockReset();
  pushToast.mockReset();
  createStub.mutate.mockReset();
  startStub.mutate.mockReset();
  deleteStub.mutate.mockReset();
  useComposerSeedStore.setState({ seed: null });
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
  const { within } = await import("@testing-library/react");
  const section = screen.getByRole("region", { name: /sources/i });
  const addAll = within(section).getAllByRole("button", { name: /^add all$/i })[0];
  await user.click(addAll);
}

async function clickAddAllDestinations(user: ReturnType<typeof userEvent.setup>) {
  const { within } = await import("@testing-library/react");
  const section = await screen.findByRole("region", { name: /destinations/i });
  await user.click(within(section).getByRole("button", { name: /^add all$/i }));
}

async function switchToEdgeCandidate(user: ReturnType<typeof userEvent.setup>) {
  const toggle = await screen.findByRole("radio", { name: /edge.?candidate/i });
  await user.click(toggle);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("CampaignComposer edge_candidate — mode switch reveals knobs", () => {
  test("switching to edge_candidate mode reveals useful_latency_ms and vm_lookback_minutes inputs", async () => {
    const user = userEvent.setup();
    renderComposer();

    await screen.findByLabelText(/^title$/i);

    expect(screen.queryByLabelText(/useful latency/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/lookback window/i)).not.toBeInTheDocument();

    await switchToEdgeCandidate(user);

    expect(await screen.findByLabelText(/useful latency/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/lookback window/i)).toBeInTheDocument();
  });

  test("Direct only (0) segment appears for edge_candidate max_hops", async () => {
    const user = userEvent.setup();
    renderComposer();

    await screen.findByLabelText(/^title$/i);

    expect(screen.queryByRole("radio", { name: /direct only/i })).not.toBeInTheDocument();

    await switchToEdgeCandidate(user);

    expect(await screen.findByRole("radio", { name: /direct only/i })).toBeInTheDocument();
  });

  test("max_hops caption disappears in edge_candidate mode", async () => {
    const user = userEvent.setup();
    renderComposer();

    await screen.findByLabelText(/^title$/i);

    expect(screen.getByText(/2 hops considers an additional mesh agent/i)).toBeInTheDocument();

    await switchToEdgeCandidate(user);

    await waitFor(() => {
      expect(
        screen.queryByText(/2 hops considers an additional mesh agent/i),
      ).not.toBeInTheDocument();
    });
  });
});

describe("CampaignComposer edge_candidate — useful_latency_ms required validation", () => {
  test("Start button is disabled when useful_latency_ms is blank in edge_candidate mode", async () => {
    setupHooks({
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Edge test");
    await selectAllSources(user);
    await clickAddAllDestinations(user);

    await switchToEdgeCandidate(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    expect(startButton).toBeDisabled();
    expect(createStub.mutate).not.toHaveBeenCalled();
  });

  test("Start button enables once useful_latency_ms is filled in", async () => {
    setupHooks({
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Edge test");
    await selectAllSources(user);
    await clickAddAllDestinations(user);

    await switchToEdgeCandidate(user);

    const latencyInput = await screen.findByLabelText(/useful latency/i);
    await user.clear(latencyInput);
    await user.type(latencyInput, "80");

    await waitFor(() => {
      const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
      expect(startButton).toBeEnabled();
    });
  });
});

describe("CampaignComposer edge_candidate — mode-aware sub-panel content", () => {
  test("max_hops caption present for diversity mode", async () => {
    setupHooks();
    renderComposer();

    await screen.findByLabelText(/^title$/i);

    expect(screen.getByText(/2 hops considers an additional mesh agent/i)).toBeInTheDocument();
  });

  test("max_hops caption present for optimization mode", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    await screen.findByLabelText(/^title$/i);

    const optimizationToggle = screen.getByRole("radio", { name: /optimization/i });
    await user.click(optimizationToggle);

    expect(screen.getByText(/2 hops considers an additional mesh agent/i)).toBeInTheDocument();
  });

  test("source explainer paragraph only appears in edge_candidate mode", async () => {
    setupHooks();
    const user = userEvent.setup();
    renderComposer();

    await screen.findByLabelText(/^title$/i);

    expect(
      screen.queryByText(/selected source agents probe each candidate/i),
    ).not.toBeInTheDocument();

    await switchToEdgeCandidate(user);

    expect(
      await screen.findByText(/selected source agents probe each candidate/i),
    ).toBeInTheDocument();
  });
});

describe("CampaignComposer edge_candidate — wire shape on submit", () => {
  test("POST body includes useful_latency_ms, max_hops, vm_lookback_minutes, evaluation_mode edge_candidate", async () => {
    setupHooks({
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "EC Campaign");
    await selectAllSources(user);
    await clickAddAllDestinations(user);

    await switchToEdgeCandidate(user);

    const latencyInput = await screen.findByLabelText(/useful latency/i);
    await user.clear(latencyInput);
    await user.type(latencyInput, "80");

    const startButton = await screen.findByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => expect(createStub.mutate).toHaveBeenCalled());

    const body = createStub.mutate.mock.calls[0]?.[0];
    expect(body).toMatchObject({
      evaluation_mode: "edge_candidate",
      useful_latency_ms: 80,
      max_hops: 2,
      vm_lookback_minutes: 15,
    });
  });

  test("POST body omits useful_latency_ms when mode is not edge_candidate", async () => {
    setupHooks({
      preview: { fresh: 10, reusable: 0, total: 10 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    await fillTitle(user, "Standard Campaign");
    await selectAllSources(user);
    await clickAddAllDestinations(user);

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => expect(createStub.mutate).toHaveBeenCalled());

    const body = createStub.mutate.mock.calls[0]?.[0];
    expect(body.useful_latency_ms).toBeUndefined();
    expect(body).toMatchObject({ max_hops: 2, vm_lookback_minutes: 15 });
  });

  test("seeding composer from edge_candidate clone restores evaluation_mode and useful_latency_ms", async () => {
    useComposerSeedStore.getState().setSeed({
      knobs: {
        ...DEFAULT_KNOBS,
        title: "Cloned EC",
        evaluation_mode: "edge_candidate",
        useful_latency_ms: 120,
        max_hops: 0,
        vm_lookback_minutes: 30,
      },
      sourceSet: ["a1"],
      destSet: ["10.1.1.1"],
    });

    setupHooks({
      preview: { fresh: 1, reusable: 0, total: 1 } as PreviewDispatchResponse,
    });
    const user = userEvent.setup();
    renderComposer();

    const titleInput = (await screen.findByLabelText(/^title$/i)) as HTMLInputElement;
    await waitFor(() => {
      expect(titleInput.value).toBe("Cloned EC");
    });

    const latencyInput = (await screen.findByLabelText(/useful latency/i)) as HTMLInputElement;
    expect(latencyInput.value).toBe("120");

    const startButton = screen.getByRole("button", { name: /^start(ing…)?$/i });
    await user.click(startButton);

    await waitFor(() => expect(createStub.mutate).toHaveBeenCalled());
    const body = createStub.mutate.mock.calls[0]?.[0];
    expect(body).toMatchObject({
      evaluation_mode: "edge_candidate",
      useful_latency_ms: 120,
      max_hops: 0,
      vm_lookback_minutes: 30,
    });
  });
});
