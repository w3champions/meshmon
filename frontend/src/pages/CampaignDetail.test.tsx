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
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { Campaign, CampaignState, PreviewDispatchResponse } from "@/api/hooks/campaigns";

// ---------------------------------------------------------------------------
// Module mocks. Register BEFORE importing the component under test so the
// real hooks never resolve.
// ---------------------------------------------------------------------------

// Spy on `useNavigate` so the delete-success navigation can be asserted
// without spinning up a navigation observer. Preserves the rest of
// `@tanstack/react-router` so `createRouter`, `useParams`, etc. still work.
const navigate = vi.fn();
vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return { ...actual, useNavigate: () => navigate };
});

vi.mock("@/api/hooks/campaigns", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/campaigns")>("@/api/hooks/campaigns");
  return {
    ...actual,
    useCampaign: vi.fn(),
    useCampaignPairs: vi.fn(),
    usePreviewDispatchCount: vi.fn(),
    useStartCampaign: vi.fn(),
    useStopCampaign: vi.fn(),
    useDeleteCampaign: vi.fn(),
    usePatchCampaign: vi.fn(),
    useEditCampaign: vi.fn(),
  };
});

vi.mock("@/api/hooks/campaign-stream", () => ({
  useCampaignStream: vi.fn(),
}));

// Spy on the toast store so Clone's cap-warning path is observable.
const pushToast = vi.fn();
vi.mock("@/stores/toast", () => ({
  useToastStore: { getState: () => ({ pushToast }) },
}));

// The EditMetadataSheet mount renders a radix portal. Stub it to a
// predictable data-testid marker that echoes the incoming campaign id so
// the test can assert the integration point without dragging the radix
// popover machinery into jsdom.
vi.mock("@/components/campaigns/EditMetadataSheet", () => ({
  EditMetadataSheet: ({ campaign, open }: { campaign: { id: string } | null; open: boolean }) =>
    open && campaign ? <div data-testid={`metadata-sheet-${campaign.id}`} /> : null,
}));

// Replace each sub-tab with a marker component whose mount is observable in
// the DOM. Lazy-mount discipline (Task 11) requires `TabsContent` to render
// only the active tab's child; the stubs let us assert that exactly one
// panel's marker is present at a time.
vi.mock("@/components/campaigns/results/CandidatesTab", () => ({
  CandidatesTab: ({ campaign }: { campaign: { id: string } }) => (
    <div data-testid={`stub-candidates-${campaign.id}`} />
  ),
}));
vi.mock("@/components/campaigns/results/PairsTab", () => ({
  PairsTab: ({ campaign }: { campaign: { id: string } }) => (
    <div data-testid={`stub-pairs-${campaign.id}`} />
  ),
}));
vi.mock("@/components/campaigns/results/RawTab", () => ({
  RawTab: ({ campaign }: { campaign: { id: string } }) => (
    <div data-testid={`stub-raw-${campaign.id}`} />
  ),
}));
vi.mock("@/components/campaigns/results/SettingsTab", () => ({
  SettingsTab: ({ campaign }: { campaign: { id: string } }) => (
    <div data-testid={`stub-settings-${campaign.id}`} />
  ),
}));

// ---------------------------------------------------------------------------
// Imports AFTER mocks so vi.fn() stubs are in place.
// ---------------------------------------------------------------------------

import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  type CampaignPair,
  useCampaign,
  useCampaignPairs,
  useDeleteCampaign,
  useEditCampaign,
  usePatchCampaign,
  usePreviewDispatchCount,
  useStartCampaign,
  useStopCampaign,
} from "@/api/hooks/campaigns";
import CampaignDetail from "@/pages/CampaignDetail";
import { campaignDetailSearchSchema } from "@/router/index";
import { useComposerSeedStore } from "@/stores/composer-seed";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

function makeCampaign(overrides: Partial<Campaign> & { state: CampaignState }): Campaign {
  return {
    id: overrides.id ?? CAMPAIGN_ID,
    title: overrides.title ?? "Campaign alpha",
    notes: overrides.notes ?? "Some notes",
    state: overrides.state,
    protocol: overrides.protocol ?? "icmp",
    evaluation_mode: overrides.evaluation_mode ?? "diversity",
    force_measurement: overrides.force_measurement ?? false,
    loss_threshold_pct: overrides.loss_threshold_pct ?? 5,
    stddev_weight: overrides.stddev_weight ?? 1,
    probe_count: overrides.probe_count ?? 10,
    probe_count_detail: overrides.probe_count_detail ?? 250,
    probe_stagger_ms: overrides.probe_stagger_ms ?? 100,
    timeout_ms: overrides.timeout_ms ?? 2000,
    created_at: overrides.created_at ?? "2026-04-01T12:00:00Z",
    created_by: overrides.created_by ?? "alice",
    started_at: overrides.started_at ?? null,
    stopped_at: overrides.stopped_at ?? null,
    completed_at: overrides.completed_at ?? null,
    evaluated_at: overrides.evaluated_at ?? null,
    pair_counts: overrides.pair_counts ?? [],
  };
}

function makeMutationStub() {
  return { mutate: vi.fn(), mutateAsync: vi.fn(), isPending: false, reset: vi.fn() };
}

const startMutationStub = makeMutationStub();
const stopMutationStub = makeMutationStub();
const deleteMutationStub = makeMutationStub();
const patchMutationStub = makeMutationStub();
const editMutationStub = makeMutationStub();

interface HookSetupOptions {
  campaign?: Campaign | null;
  isLoading?: boolean;
  isError?: boolean;
  error?: Error | null;
  refetch?: ReturnType<typeof vi.fn>;
  preview?: PreviewDispatchResponse;
  pairs?: CampaignPair[];
  pairsIsLoading?: boolean;
  pairsIsError?: boolean;
  pairsRefetch?: ReturnType<typeof vi.fn>;
}

function setupHookMocks(opts: HookSetupOptions = {}) {
  vi.mocked(useCampaign).mockReturnValue({
    data: opts.campaign ?? null,
    isLoading: opts.isLoading ?? false,
    isError: opts.isError ?? false,
    error: opts.error ?? null,
    refetch: opts.refetch ?? vi.fn(),
  } as unknown as ReturnType<typeof useCampaign>);

  vi.mocked(useCampaignPairs).mockReturnValue({
    data: opts.pairs,
    isLoading: opts.pairsIsLoading ?? false,
    isError: opts.pairsIsError ?? false,
    refetch: opts.pairsRefetch ?? vi.fn(),
  } as unknown as ReturnType<typeof useCampaignPairs>);

  vi.mocked(usePreviewDispatchCount).mockReturnValue({
    data: opts.preview,
    isLoading: opts.preview === undefined,
    isError: false,
  } as unknown as ReturnType<typeof usePreviewDispatchCount>);

  vi.mocked(useStartCampaign).mockReturnValue(
    startMutationStub as unknown as ReturnType<typeof useStartCampaign>,
  );
  vi.mocked(useStopCampaign).mockReturnValue(
    stopMutationStub as unknown as ReturnType<typeof useStopCampaign>,
  );
  vi.mocked(useDeleteCampaign).mockReturnValue(
    deleteMutationStub as unknown as ReturnType<typeof useDeleteCampaign>,
  );
  vi.mocked(usePatchCampaign).mockReturnValue(
    patchMutationStub as unknown as ReturnType<typeof usePatchCampaign>,
  );
  vi.mocked(useEditCampaign).mockReturnValue(
    editMutationStub as unknown as ReturnType<typeof useEditCampaign>,
  );
  vi.mocked(useCampaignStream).mockReturnValue(undefined);
}

// ---------------------------------------------------------------------------
// Router harness — registers the detail route under the same path the real
// router uses so `campaignDetailRoute.useParams()` resolves against the
// initial memory history entry.
// ---------------------------------------------------------------------------

interface RenderOptions {
  /** Trailing query string (including the leading `?`) appended to the initial path. */
  search?: string;
}

function renderDetail(opts: RenderOptions = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });

  const rootRoute = createRootRoute({ component: Outlet });
  const detailRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns/$id",
    component: CampaignDetail,
    // Mirror the production `validateSearch` so `?tab=…` is parsed with
    // the per-field `.catch` behaviour: an invalid value on one field
    // bounces to that field's default without dropping its valid siblings.
    // TanStack Router merges the validator's output onto the raw search,
    // so we explicitly set every known key on the return — explicit
    // `undefined` deletes stale keys instead of leaving them on the URL.
    validateSearch: (raw: Record<string, unknown>) => {
      const result = campaignDetailSearchSchema.safeParse(raw);
      const parsed = result.success ? result.data : { tab: "candidates" as const };
      return {
        tab: parsed.tab ?? ("candidates" as const),
        raw_state: parsed.raw_state,
        raw_protocol: parsed.raw_protocol,
        raw_kind: parsed.raw_kind,
      };
    },
  });
  const listRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns",
    component: () => null,
  });

  const router = createRouter({
    routeTree: rootRoute.addChildren([detailRoute, listRoute]),
    history: createMemoryHistory({
      initialEntries: [`/campaigns/${CAMPAIGN_ID}${opts.search ?? ""}`],
    }),
  });

  const result = render(
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, client };
}

beforeEach(() => {
  navigate.mockReset();
  pushToast.mockReset();
  startMutationStub.mutate.mockReset();
  stopMutationStub.mutate.mockReset();
  deleteMutationStub.mutate.mockReset();
  patchMutationStub.mutate.mockReset();
  editMutationStub.mutate.mockReset();
  // Reset the composer-seed store between tests — Zustand state is a
  // module-level singleton, so a leftover seed from one test would leak
  // into the next.
  useComposerSeedStore.setState({ seed: null });
  setupHookMocks();
});

afterEach(() => {
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("CampaignDetail — pair counts", () => {
  test("renders all provided counts with accessible labels", async () => {
    setupHookMocks({
      campaign: makeCampaign({
        state: "running",
        pair_counts: [
          ["pending", 5],
          ["succeeded", 12],
          ["unreachable", 2],
        ],
      }),
    });

    renderDetail();

    await screen.findByRole("heading", { name: /campaign alpha/i });

    expect(screen.getByLabelText(/^pending: 5$/)).toBeInTheDocument();
    expect(screen.getByLabelText(/^succeeded: 12$/)).toBeInTheDocument();
    expect(screen.getByLabelText(/^unreachable: 2$/)).toBeInTheDocument();
    // Unreported states zero-fill rather than dropping off the strip.
    expect(screen.getByLabelText(/^dispatched: 0$/)).toBeInTheDocument();
    expect(screen.getByLabelText(/^skipped: 0$/)).toBeInTheDocument();
  });
});

describe("CampaignDetail — state-gated action bar", () => {
  test("Start button appears only on draft", async () => {
    setupHookMocks({ campaign: makeCampaign({ state: "draft" }) });
    renderDetail();

    await screen.findByRole("button", { name: /^start$/i });
    expect(screen.queryByRole("button", { name: /^stop$/i })).not.toBeInTheDocument();
  });

  test("Stop button replaces Start on running campaigns", async () => {
    // Fresh mount with `state: "running"`. Swapping hook state via
    // rerender won't re-run mocked hooks against a new return, so a
    // clean remount is the reliable way to verify the alternate branch.
    setupHookMocks({ campaign: makeCampaign({ state: "running" }) });
    renderDetail();

    await screen.findByRole("button", { name: /^stop$/i });
    expect(screen.queryByRole("button", { name: /^start$/i })).not.toBeInTheDocument();
  });
});

describe("CampaignDetail — Stop mutation + state-change rerender", () => {
  test("Stop click fires mutation and badge updates when campaign state changes", async () => {
    // --- Part 1: initial running render, click Stop, assert mutation ----
    setupHookMocks({ campaign: makeCampaign({ state: "running" }) });
    const user = userEvent.setup();
    renderDetail();

    const stopButton = await screen.findByRole("button", { name: /^stop$/i });
    await user.click(stopButton);
    expect(stopMutationStub.mutate).toHaveBeenCalledWith(CAMPAIGN_ID, expect.any(Object));

    // --- Part 2: tear down and re-mount with the stopped campaign ------
    // The actual SSE-driven cache invalidation is tested in the
    // `useCampaignStream` spec. Here we only verify that the component
    // re-renders correctly once the hook returns a stopped campaign —
    // regardless of transport. Unmount and remount cleanly so the
    // mocked hook is re-invoked with its new return value.
    cleanup();
    setupHookMocks({ campaign: makeCampaign({ state: "stopped" }) });
    renderDetail();

    await waitFor(() => {
      expect(screen.getByLabelText(/state: stopped/i)).toBeInTheDocument();
    });
    expect(screen.queryByRole("button", { name: /^stop$/i })).not.toBeInTheDocument();
  });
});

describe("CampaignDetail — Restart action on terminal campaigns", () => {
  test("Restart button appears on completed campaigns and fires empty-body edit", async () => {
    // Completed is a terminal state, so Restart surfaces; Start/Stop don't.
    setupHookMocks({ campaign: makeCampaign({ state: "completed" }) });
    const user = userEvent.setup();
    renderDetail();

    const restart = await screen.findByRole("button", { name: /^restart$/i });
    expect(screen.queryByRole("button", { name: /^start$/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^stop$/i })).not.toBeInTheDocument();

    await user.click(restart);
    // Empty-body contract — re-enters `running` without resetting pairs.
    expect(editMutationStub.mutate).toHaveBeenCalledWith(
      { id: CAMPAIGN_ID, body: {} },
      expect.any(Object),
    );
  });

  test("Restart is absent on draft and running campaigns", async () => {
    setupHookMocks({ campaign: makeCampaign({ state: "draft" }) });
    renderDetail();
    await screen.findByRole("button", { name: /^start$/i });
    expect(screen.queryByRole("button", { name: /^restart$/i })).not.toBeInTheDocument();

    cleanup();
    setupHookMocks({ campaign: makeCampaign({ state: "running" }) });
    renderDetail();
    await screen.findByRole("button", { name: /^stop$/i });
    expect(screen.queryByRole("button", { name: /^restart$/i })).not.toBeInTheDocument();
  });
});

describe("CampaignDetail — Clone action", () => {
  function makePair(overrides: Partial<CampaignPair> = {}): CampaignPair {
    return {
      id: overrides.id ?? 1,
      campaign_id: overrides.campaign_id ?? CAMPAIGN_ID,
      source_agent_id: overrides.source_agent_id ?? "agent-1",
      destination_ip: overrides.destination_ip ?? "10.0.0.1",
      resolution_state: overrides.resolution_state ?? "succeeded",
      measurement_id: overrides.measurement_id ?? null,
      dispatched_at: overrides.dispatched_at ?? null,
      settled_at: overrides.settled_at ?? null,
      attempt_count: overrides.attempt_count ?? 1,
      last_error: overrides.last_error ?? null,
    };
  }

  test("Clone button appears on terminal campaigns and seeds the composer", async () => {
    const pairs = [
      makePair({ id: 1, source_agent_id: "agent-a", destination_ip: "10.0.0.1" }),
      makePair({ id: 2, source_agent_id: "agent-a", destination_ip: "10.0.0.2" }),
      makePair({ id: 3, source_agent_id: "agent-b", destination_ip: "10.0.0.1" }),
    ];
    setupHookMocks({
      campaign: makeCampaign({
        state: "completed",
        title: "Alpha",
        notes: "hello",
        protocol: "icmp",
      }),
      pairs,
    });
    const user = userEvent.setup();
    renderDetail();

    const cloneBtn = await screen.findByRole("button", { name: /clone campaign/i });
    await user.click(cloneBtn);

    const seed = useComposerSeedStore.getState().seed;
    expect(seed).not.toBeNull();
    // Dedupe invariant: 3 pair rows collapse to 2 sources and 2 destinations.
    expect(seed?.sourceSet).toEqual(expect.arrayContaining(["agent-a", "agent-b"]));
    expect(seed?.sourceSet).toHaveLength(2);
    expect(seed?.destSet).toEqual(expect.arrayContaining(["10.0.0.1", "10.0.0.2"]));
    expect(seed?.destSet).toHaveLength(2);
    expect(seed?.knobs.title).toBe("Copy of Alpha");
    expect(seed?.knobs.notes).toBe("hello");
    expect(seed?.knobs.force_measurement).toBe(false);

    expect(navigate).toHaveBeenCalledWith({ to: "/campaigns/new" });
  });

  test("Clone button is absent on running campaigns", async () => {
    setupHookMocks({ campaign: makeCampaign({ state: "running" }) });
    renderDetail();

    await screen.findByRole("button", { name: /^stop$/i });
    expect(screen.queryByRole("button", { name: /clone campaign/i })).not.toBeInTheDocument();
  });

  test("Clone button is absent on draft campaigns", async () => {
    setupHookMocks({ campaign: makeCampaign({ state: "draft" }) });
    renderDetail();

    await screen.findByRole("button", { name: /^start$/i });
    expect(screen.queryByRole("button", { name: /clone campaign/i })).not.toBeInTheDocument();
  });

  test("Clone toasts a warning when the pair list hits the 5000 cap", async () => {
    // Synthetic 5 000-row pair list. Using small `% 50` cycles keeps
    // dedup dense so the source/destination sets don't explode — the
    // test only cares about the cap-warning toast, not the seed shape.
    const pairs = Array.from({ length: 5000 }, (_, i) =>
      makePair({
        id: i + 1,
        source_agent_id: `agent-${i % 50}`,
        destination_ip: `10.${Math.floor(i / 256)}.${(i % 256) + 1}.1`,
      }),
    );
    setupHookMocks({ campaign: makeCampaign({ state: "completed" }), pairs });

    const user = userEvent.setup();
    renderDetail();

    const cloneBtn = await screen.findByRole("button", { name: /clone campaign/i });
    await user.click(cloneBtn);

    expect(pushToast).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "error",
        message: expect.stringMatching(/5,?000\+\s+pairs.*truncated/i),
      }),
    );
  });

  test("Clone button is disabled until pairs have loaded", async () => {
    // Omit `pairs` in setup — the mocked useCampaignPairs returns `data:
    // undefined`, which the component reads as "still loading".
    setupHookMocks({ campaign: makeCampaign({ state: "completed" }) });
    renderDetail();

    const cloneBtn = await screen.findByRole("button", { name: /clone campaign/i });
    expect(cloneBtn).toBeDisabled();
  });

  test("Clone button is disabled on zero-pair terminal campaigns", async () => {
    // A campaign that resolved to an empty pair list is not a useful
    // Clone seed — no sources, no destinations to copy. The button
    // stays disabled even though the query technically resolved,
    // because `pairsLoaded` gates on a non-empty array.
    setupHookMocks({ campaign: makeCampaign({ state: "completed" }), pairs: [] });
    const user = userEvent.setup();
    renderDetail();

    const cloneBtn = await screen.findByRole("button", { name: /clone campaign/i });
    expect(cloneBtn).toBeDisabled();

    // Attempting to click a disabled button must not seed the composer.
    await user.click(cloneBtn);
    expect(useComposerSeedStore.getState().seed).toBeNull();
  });

  test("Clone button shows a loading label while pairs fetch", async () => {
    // `pairsIsLoading: true` mimics the initial fetch state before any
    // data arrives. The label should flip to "Clone (loading…)" to
    // match the Start/Stop `isPending`-style idiom, and the button
    // stays disabled until the data resolves.
    setupHookMocks({
      campaign: makeCampaign({ state: "completed" }),
      pairsIsLoading: true,
    });
    renderDetail();

    const cloneBtn = await screen.findByRole("button", { name: /clone campaign/i });
    expect(cloneBtn).toBeDisabled();
    expect(cloneBtn).toHaveTextContent(/loading/i);
  });

  test("Clone button exposes a retry affordance when pairs fail to load", async () => {
    // When the pair fetch errors (network / 5xx), the operator needs
    // visible feedback. The button flips to "Clone (retry)" and
    // clicking it refetches rather than seeding the composer.
    const pairsRefetch = vi.fn();
    setupHookMocks({
      campaign: makeCampaign({ state: "completed" }),
      pairsIsError: true,
      pairsRefetch,
    });
    const user = userEvent.setup();
    renderDetail();

    const cloneBtn = await screen.findByRole("button", { name: /clone campaign/i });
    expect(cloneBtn).toBeEnabled();
    expect(cloneBtn).toHaveTextContent(/retry/i);

    await user.click(cloneBtn);
    // Retry path wires to `refetch`, not the composer-seed handoff.
    expect(pairsRefetch).toHaveBeenCalledTimes(1);
    expect(useComposerSeedStore.getState().seed).toBeNull();
    expect(navigate).not.toHaveBeenCalledWith({ to: "/campaigns/new" });
  });
});

describe("CampaignDetail — Edit metadata sheet", () => {
  test("Edit metadata button renders EditMetadataSheet with the campaign id", async () => {
    setupHookMocks({ campaign: makeCampaign({ state: "running" }) });
    const user = userEvent.setup();
    renderDetail();

    const editBtn = await screen.findByRole("button", { name: /edit metadata/i });
    await user.click(editBtn);

    await waitFor(() => {
      expect(screen.getByTestId(`metadata-sheet-${CAMPAIGN_ID}`)).toBeInTheDocument();
    });
  });
});

describe("CampaignDetail — loading skeleton", () => {
  test("renders a status skeleton while the campaign query is loading", async () => {
    setupHookMocks({ campaign: null, isLoading: true });
    renderDetail();

    expect(await screen.findByRole("status")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: /campaign alpha/i })).not.toBeInTheDocument();
  });
});

describe("CampaignDetail — 404 not found", () => {
  test("renders a not-found notice with a back-link to /campaigns", async () => {
    // `useCampaign` returns `null` for 404 (distinct from still-loading).
    setupHookMocks({ campaign: null, isLoading: false, isError: false });
    renderDetail();

    expect(await screen.findByRole("heading", { name: /campaign not found/i })).toBeInTheDocument();
    const backLink = screen.getByRole("link", { name: /back to campaigns/i });
    expect(backLink).toHaveAttribute("href", "/campaigns");
  });
});

describe("CampaignDetail — error state", () => {
  test("renders a retry affordance that re-invokes the query", async () => {
    const refetch = vi.fn();
    setupHookMocks({
      campaign: null,
      isLoading: false,
      isError: true,
      error: new Error("net fail"),
      refetch,
    });
    const user = userEvent.setup();
    renderDetail();

    expect(
      await screen.findByRole("heading", { name: /failed to load campaign/i }),
    ).toBeInTheDocument();

    const retry = screen.getByRole("button", { name: /retry/i });
    await user.click(retry);
    expect(refetch).toHaveBeenCalledTimes(1);
  });
});

describe("CampaignDetail — tab shell", () => {
  test("defaults to the Candidates tab when no ?tab param is present", async () => {
    setupHookMocks({ campaign: makeCampaign({ state: "completed" }) });
    renderDetail();

    // Lazy-mount discipline: only the active tab's sub-component is rendered.
    expect(await screen.findByTestId(`stub-candidates-${CAMPAIGN_ID}`)).toBeInTheDocument();
    expect(screen.queryByTestId(`stub-pairs-${CAMPAIGN_ID}`)).not.toBeInTheDocument();
    expect(screen.queryByTestId(`stub-raw-${CAMPAIGN_ID}`)).not.toBeInTheDocument();
    expect(screen.queryByTestId(`stub-settings-${CAMPAIGN_ID}`)).not.toBeInTheDocument();
  });

  test("renders only the Raw panel when the URL requests ?tab=raw", async () => {
    setupHookMocks({ campaign: makeCampaign({ state: "running" }) });
    renderDetail({ search: "?tab=raw" });

    // Raw tab active → raw stub is mounted, every other tab stub is absent.
    expect(await screen.findByTestId(`stub-raw-${CAMPAIGN_ID}`)).toBeInTheDocument();
    expect(screen.queryByTestId(`stub-candidates-${CAMPAIGN_ID}`)).not.toBeInTheDocument();
    expect(screen.queryByTestId(`stub-pairs-${CAMPAIGN_ID}`)).not.toBeInTheDocument();
    expect(screen.queryByTestId(`stub-settings-${CAMPAIGN_ID}`)).not.toBeInTheDocument();
  });

  test("falls back to Candidates when the URL requests an unknown tab", async () => {
    // Per-field `.catch` on the schema's `tab` enum bounces invalid values
    // to the default `"candidates"` — the schema default now lives on the
    // field itself (rather than a whole-object `.catch({})`).
    setupHookMocks({ campaign: makeCampaign({ state: "running" }) });
    renderDetail({ search: "?tab=bogus" });

    expect(await screen.findByTestId(`stub-candidates-${CAMPAIGN_ID}`)).toBeInTheDocument();
    expect(screen.queryByTestId(`stub-raw-${CAMPAIGN_ID}`)).not.toBeInTheDocument();
  });

  test("preserves a valid raw_state when tab is invalid (per-field catch)", async () => {
    // Regression for the whole-object `.catch({})` bug: previously an
    // invalid `tab=bogus` would reset the ENTIRE search object, dropping
    // the valid `raw_state=pending` along with the bad tab. Per-field
    // `.catch` on the schema keeps `raw_state` intact and only bounces
    // `tab` back to `"candidates"`.
    setupHookMocks({ campaign: makeCampaign({ state: "running" }) });
    const { router } = renderDetail({ search: "?tab=bogus&raw_state=pending" });

    // The page mounts Candidates (tab fell back), but `raw_state` survives
    // on the router's parsed search bag.
    expect(await screen.findByTestId(`stub-candidates-${CAMPAIGN_ID}`)).toBeInTheDocument();
    const routerSearch = router.state.location.search as {
      tab?: string;
      raw_state?: string;
    };
    expect(routerSearch.tab).toBe("candidates");
    expect(routerSearch.raw_state).toBe("pending");
  });

  test("invalid raw_protocol falls back to undefined without dropping tab=raw", async () => {
    // Mirror of the bug above for the `raw_protocol` field — `?tab=raw`
    // must stay intact while `raw_protocol=banana` is silently dropped by
    // the per-field `.catch`.
    setupHookMocks({ campaign: makeCampaign({ state: "running" }) });
    const { router } = renderDetail({ search: "?tab=raw&raw_protocol=banana" });

    expect(await screen.findByTestId(`stub-raw-${CAMPAIGN_ID}`)).toBeInTheDocument();
    const routerSearch = router.state.location.search as {
      tab?: string;
      raw_protocol?: string;
    };
    expect(routerSearch.tab).toBe("raw");
    expect(routerSearch.raw_protocol).toBeUndefined();
  });

  test("clicking the Settings trigger asks the router to persist ?tab=settings", async () => {
    setupHookMocks({ campaign: makeCampaign({ state: "completed" }) });
    const user = userEvent.setup();
    renderDetail();

    await screen.findByTestId(`stub-candidates-${CAMPAIGN_ID}`);
    await user.click(screen.getByRole("tab", { name: /evaluation settings/i }));

    // The page invokes `navigate({ search: { ...search, tab: "settings" }, replace: true })`
    // so the router's `validateSearch` + the URL stay in lockstep. We can't
    // observe the panel swap here because `useNavigate` is mocked at the
    // module level; asserting the navigate payload is sufficient evidence
    // that the tab shell is driving the URL as designed.
    await waitFor(() => {
      expect(navigate).toHaveBeenCalled();
    });
    const lastCall = navigate.mock.calls[navigate.mock.calls.length - 1]?.[0] as {
      search?: { tab?: string };
      replace?: boolean;
    };
    expect(lastCall?.search?.tab).toBe("settings");
    expect(lastCall?.replace).toBe(true);
  });
});

describe("CampaignDetail — delete success", () => {
  test("confirming Delete fires the mutation and navigates back to /campaigns", async () => {
    // Wire the delete stub so the mutation handler's `onSuccess` callback
    // fires synchronously — this drives the navigate call the test asserts.
    deleteMutationStub.mutate.mockImplementation(
      (_id: string, opts?: { onSuccess?: () => void }) => {
        opts?.onSuccess?.();
      },
    );
    setupHookMocks({ campaign: makeCampaign({ state: "draft" }) });
    const user = userEvent.setup();
    renderDetail();

    // Open the confirm dialog, then click its destructive Delete button.
    const deleteBtn = await screen.findByRole("button", { name: /^delete$/i });
    await user.click(deleteBtn);

    const dialog = await screen.findByRole("alertdialog");
    await user.click(within(dialog).getByRole("button", { name: /^delete$/i }));

    expect(deleteMutationStub.mutate).toHaveBeenCalledWith(CAMPAIGN_ID, expect.any(Object));
    await waitFor(() => {
      expect(navigate).toHaveBeenCalledWith({ to: "/campaigns" });
    });
  });
});
