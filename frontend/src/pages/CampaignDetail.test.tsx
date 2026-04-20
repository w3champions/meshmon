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

// The EditMetadataSheet mount renders a radix portal. Stub it to a
// predictable data-testid marker that echoes the incoming campaign id so
// the test can assert the integration point without dragging the radix
// popover machinery into jsdom.
vi.mock("@/components/campaigns/EditMetadataSheet", () => ({
  EditMetadataSheet: ({ campaign, open }: { campaign: { id: string } | null; open: boolean }) =>
    open && campaign ? <div data-testid={`metadata-sheet-${campaign.id}`} /> : null,
}));

// Short-circuit EditPairsSheet for the same reason.
vi.mock("@/components/campaigns/EditPairsSheet", () => ({
  EditPairsSheet: ({ campaign, open }: { campaign: { id: string } | null; open: boolean }) =>
    open && campaign ? <div data-testid={`pairs-sheet-${campaign.id}`} /> : null,
}));

// ---------------------------------------------------------------------------
// Imports AFTER mocks so vi.fn() stubs are in place.
// ---------------------------------------------------------------------------

import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  useCampaign,
  useDeleteCampaign,
  useEditCampaign,
  usePatchCampaign,
  usePreviewDispatchCount,
  useStartCampaign,
  useStopCampaign,
} from "@/api/hooks/campaigns";
import CampaignDetail from "@/pages/CampaignDetail";

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
}

function setupHookMocks(opts: HookSetupOptions = {}) {
  vi.mocked(useCampaign).mockReturnValue({
    data: opts.campaign ?? null,
    isLoading: opts.isLoading ?? false,
    isError: opts.isError ?? false,
    error: opts.error ?? null,
    refetch: opts.refetch ?? vi.fn(),
  } as unknown as ReturnType<typeof useCampaign>);

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

function renderDetail() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });

  const rootRoute = createRootRoute({ component: Outlet });
  const detailRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns/$id",
    component: CampaignDetail,
  });
  const listRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns",
    component: () => null,
  });

  const router = createRouter({
    routeTree: rootRoute.addChildren([detailRoute, listRoute]),
    history: createMemoryHistory({ initialEntries: [`/campaigns/${CAMPAIGN_ID}`] }),
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
  startMutationStub.mutate.mockReset();
  stopMutationStub.mutate.mockReset();
  deleteMutationStub.mutate.mockReset();
  patchMutationStub.mutate.mockReset();
  editMutationStub.mutate.mockReset();
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
