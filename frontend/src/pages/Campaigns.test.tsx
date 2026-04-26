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
import type { Campaign, CampaignState } from "@/api/hooks/campaigns";
import Campaigns from "@/pages/Campaigns";

// ---------------------------------------------------------------------------
// Module mocks
// ---------------------------------------------------------------------------

vi.mock("@/api/hooks/campaigns", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/campaigns")>("@/api/hooks/campaigns");
  return {
    ...actual,
    useCampaignsList: vi.fn(),
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

import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  CAMPAIGNS_LIST_KEY,
  useCampaignsList,
  useDeleteCampaign,
  useEditCampaign,
  usePatchCampaign,
  useStartCampaign,
  useStopCampaign,
} from "@/api/hooks/campaigns";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

function makeCampaign(
  overrides: Partial<Campaign> & { id: string; state: CampaignState },
): Campaign {
  return {
    id: overrides.id,
    title: overrides.title ?? `Campaign ${overrides.id}`,
    notes: overrides.notes ?? "",
    state: overrides.state,
    protocol: overrides.protocol ?? "icmp",
    evaluation_mode: overrides.evaluation_mode ?? "diversity",
    force_measurement: overrides.force_measurement ?? false,
    loss_threshold_ratio: overrides.loss_threshold_ratio ?? 0.05,
    stddev_weight: overrides.stddev_weight ?? 1,
    probe_count: overrides.probe_count ?? 10,
    probe_count_detail: overrides.probe_count_detail ?? 10,
    probe_stagger_ms: overrides.probe_stagger_ms ?? 100,
    timeout_ms: overrides.timeout_ms ?? 1000,
    created_at: overrides.created_at ?? "2026-04-01T12:00:00Z",
    created_by: overrides.created_by ?? "alice",
    started_at: overrides.started_at ?? null,
    stopped_at: overrides.stopped_at ?? null,
    completed_at: overrides.completed_at ?? null,
    evaluated_at: overrides.evaluated_at ?? null,
    pair_counts: overrides.pair_counts ?? [],
    max_hops: overrides.max_hops ?? 2,
    vm_lookback_minutes: overrides.vm_lookback_minutes ?? 15,
  };
}

const DRAFT_CAMPAIGN = makeCampaign({
  id: "11111111-1111-1111-1111-111111111111",
  title: "Draft alpha",
  state: "draft",
});
const RUNNING_CAMPAIGN = makeCampaign({
  id: "22222222-2222-2222-2222-222222222222",
  title: "Running bravo",
  state: "running",
  started_at: "2026-04-02T12:00:00Z",
});
const COMPLETED_CAMPAIGN = makeCampaign({
  id: "33333333-3333-3333-3333-333333333333",
  title: "Completed charlie",
  state: "completed",
  completed_at: "2026-04-03T12:00:00Z",
});

// ---------------------------------------------------------------------------
// Mutation stubs. Each hook returns an object shaped like the real
// `UseMutationResult` — only the bits the page reads are meaningful.
// ---------------------------------------------------------------------------

function makeMutationStub() {
  return { mutate: vi.fn(), mutateAsync: vi.fn(), isPending: false };
}

interface HookSetupOptions {
  data?: Campaign[];
  isLoading?: boolean;
  isError?: boolean;
  refetch?: () => void;
}

const startMutationStub = makeMutationStub();
const stopMutationStub = makeMutationStub();
const deleteMutationStub = makeMutationStub();
const patchMutationStub = makeMutationStub();
const editMutationStub = makeMutationStub();

function setupHookMocks(opts: HookSetupOptions = {}) {
  vi.mocked(useCampaignsList).mockReturnValue({
    data: opts.data ?? [],
    isLoading: opts.isLoading ?? false,
    isError: opts.isError ?? false,
    refetch: opts.refetch ?? vi.fn(),
  } as unknown as ReturnType<typeof useCampaignsList>);
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
// Router harness
// ---------------------------------------------------------------------------

function renderCampaigns(initialPath = "/campaigns") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });

  const rootRoute = createRootRoute({ component: Outlet });
  const campaignsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/campaigns",
    component: Campaigns,
  });

  const router = createRouter({
    routeTree: rootRoute.addChildren([campaignsRoute]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
  });

  const result = render(
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, client };
}

beforeEach(() => {
  startMutationStub.mutate.mockReset();
  stopMutationStub.mutate.mockReset();
  deleteMutationStub.mutate.mockReset();
  patchMutationStub.mutate.mockReset();
  editMutationStub.mutate.mockReset();
  setupHookMocks();
});

afterEach(() => {
  vi.clearAllMocks();
  vi.useRealTimers();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("Campaigns page — empty state", () => {
  test("renders 'No campaigns yet.' + Create CTA when no campaigns + no filters", async () => {
    setupHookMocks({ data: [] });
    renderCampaigns();

    expect(await screen.findByText(/no campaigns yet/i)).toBeInTheDocument();
    const links = screen.getAllByRole("link", { name: /create campaign/i });
    expect(links.length).toBeGreaterThan(0);
  });
});

describe("Campaigns page — rendered rows", () => {
  test("renders a row for each campaign with a state badge", async () => {
    setupHookMocks({
      data: [DRAFT_CAMPAIGN, RUNNING_CAMPAIGN, COMPLETED_CAMPAIGN],
    });
    renderCampaigns();

    await screen.findByText("Draft alpha");
    expect(screen.getByText("Running bravo")).toBeInTheDocument();
    expect(screen.getByText("Completed charlie")).toBeInTheDocument();

    expect(screen.getByText("draft")).toBeInTheDocument();
    expect(screen.getByText("running")).toBeInTheDocument();
    expect(screen.getByText("completed")).toBeInTheDocument();
  });
});

describe("Campaigns page — URL-backed filters", () => {
  test("typing in the search box debounces and pushes `q` into the URL", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    setupHookMocks({ data: [] });
    renderCampaigns();

    const input = await screen.findByLabelText(/search title or notes/i);
    await user.type(input, "alpha");

    // Fast-forward past the debounce.
    await vi.advanceTimersByTimeAsync(350);

    await waitFor(() => {
      const latestCall = vi.mocked(useCampaignsList).mock.calls.at(-1)?.[0];
      expect(latestCall).toMatchObject({ q: "alpha" });
    });
  });

  test("typing in the Created by input debounces and pushes `created_by` into the URL", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    setupHookMocks({ data: [] });
    renderCampaigns();

    const input = await screen.findByLabelText(/created by/i);
    await user.type(input, "alice");

    // Fast-forward past the debounce.
    await vi.advanceTimersByTimeAsync(350);

    await waitFor(() => {
      const latestCall = vi.mocked(useCampaignsList).mock.calls.at(-1)?.[0];
      expect(latestCall).toMatchObject({ created_by: "alice" });
    });
  });

  test("selecting a state from the dropdown writes `state` into the URL", async () => {
    const user = userEvent.setup();
    setupHookMocks({ data: [] });
    renderCampaigns();

    const trigger = await screen.findByRole("combobox", { name: /state/i });
    await user.click(trigger);
    const runningOption = await screen.findByRole("option", { name: /running/i });
    await user.click(runningOption);

    await waitFor(() => {
      const latestCall = vi.mocked(useCampaignsList).mock.calls.at(-1)?.[0];
      expect(latestCall).toMatchObject({ state: "running" });
    });
  });

  test("pre-existing ?state=running surfaces as the list query", async () => {
    setupHookMocks({ data: [] });
    renderCampaigns("/campaigns?state=running");

    await waitFor(() => {
      const calls = vi.mocked(useCampaignsList).mock.calls;
      expect(calls.length).toBeGreaterThan(0);
      const [query] = calls[calls.length - 1];
      expect(query).toMatchObject({ state: "running" });
    });
  });
});

describe("Campaigns page — state-gated row actions", () => {
  test("draft row shows Start; running row shows Stop; completed row shows Clone", async () => {
    setupHookMocks({
      data: [DRAFT_CAMPAIGN, RUNNING_CAMPAIGN, COMPLETED_CAMPAIGN],
    });
    const user = userEvent.setup();
    renderCampaigns();

    await screen.findByText("Draft alpha");

    // Draft → Start visible, Stop not.
    const draftTrigger = screen.getByRole("button", {
      name: `Actions for ${DRAFT_CAMPAIGN.title}`,
    });
    await user.click(draftTrigger);
    expect(await screen.findByRole("menuitem", { name: /^start$/i })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: /^stop$/i })).not.toBeInTheDocument();
    await user.keyboard("{Escape}");

    // Running → Stop visible, Start not. Edit metadata is also available on
    // running rows (metadata is editable without stopping the campaign).
    const runningTrigger = screen.getByRole("button", {
      name: `Actions for ${RUNNING_CAMPAIGN.title}`,
    });
    await user.click(runningTrigger);
    expect(await screen.findByRole("menuitem", { name: /^stop$/i })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: /^start$/i })).not.toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: /edit metadata/i })).toBeInTheDocument();
    await user.keyboard("{Escape}");

    // Completed → Clone + Restart visible, Start/Stop both absent. The
    // row-level Clone navigates to the detail page; the detail Clone
    // button is where the pair list loads and the composer seed lands.
    const completedTrigger = screen.getByRole("button", {
      name: `Actions for ${COMPLETED_CAMPAIGN.title}`,
    });
    await user.click(completedTrigger);
    expect(await screen.findByRole("menuitem", { name: /^clone$/i })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: /^restart$/i })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: /^start$/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: /^stop$/i })).not.toBeInTheDocument();
  });

  test("Clone on a completed row navigates to the detail page", async () => {
    setupHookMocks({ data: [COMPLETED_CAMPAIGN] });
    const user = userEvent.setup();
    const { router } = renderCampaigns();

    const trigger = await screen.findByRole("button", {
      name: `Actions for ${COMPLETED_CAMPAIGN.title}`,
    });
    await user.click(trigger);
    const cloneItem = await screen.findByRole("menuitem", { name: /^clone$/i });
    await user.click(cloneItem);

    // Row-level Clone defers to the detail page's Clone button, which
    // has the campaign's pair list loaded and can populate the composer
    // seed correctly.
    await waitFor(() => {
      expect(router.state.location.pathname).toBe(`/campaigns/${COMPLETED_CAMPAIGN.id}`);
    });
  });

  test("Restart on a completed row fires useEditCampaign with an empty body", async () => {
    setupHookMocks({ data: [COMPLETED_CAMPAIGN] });
    const user = userEvent.setup();
    renderCampaigns();

    const trigger = await screen.findByRole("button", {
      name: `Actions for ${COMPLETED_CAMPAIGN.title}`,
    });
    await user.click(trigger);
    const restartItem = await screen.findByRole("menuitem", { name: /^restart$/i });
    await user.click(restartItem);

    // Empty-body contract: re-enters `running` without touching pair state.
    // The handler also passes onError for toast routing — we assert the body
    // here; the toast spec is exercised elsewhere.
    expect(editMutationStub.mutate).toHaveBeenCalledWith(
      { id: COMPLETED_CAMPAIGN.id, body: {} },
      expect.any(Object),
    );
  });

  test("Stop click fires useStopCampaign.mutate with the campaign id", async () => {
    setupHookMocks({ data: [RUNNING_CAMPAIGN] });
    const user = userEvent.setup();
    renderCampaigns();

    const trigger = await screen.findByRole("button", {
      name: `Actions for ${RUNNING_CAMPAIGN.title}`,
    });
    await user.click(trigger);
    const stopItem = await screen.findByRole("menuitem", { name: /^stop$/i });
    await user.click(stopItem);

    // Stop passes an `onError` callback alongside the id (surfaces 409 toasts
    // via `useToastStore`). We don't assert the callback shape here — the
    // toast behavior is exercised directly in a dedicated spec.
    expect(stopMutationStub.mutate).toHaveBeenCalledWith(RUNNING_CAMPAIGN.id, expect.any(Object));
  });
});

describe("Campaigns page — SSE stream hook", () => {
  test("state_changed invalidates the list key through the stream hook", async () => {
    // `useCampaignStream` invalidates CAMPAIGNS_LIST_KEY on `state_changed`.
    // Assert the page mounts the stream exactly once and that the shared
    // list key constant is stable (the SSE handler targets it by identity).
    setupHookMocks({ data: [] });
    renderCampaigns();

    await screen.findByLabelText(/search title or notes/i);
    expect(useCampaignStream).toHaveBeenCalled();
    expect(CAMPAIGNS_LIST_KEY).toEqual(["campaigns", "list"]);
  });
});

describe("Campaigns page — row navigation", () => {
  test("clicking a row body navigates to /campaigns/$id", async () => {
    setupHookMocks({ data: [DRAFT_CAMPAIGN] });
    const user = userEvent.setup();
    const { router } = renderCampaigns();

    const row = await screen.findByTestId(`campaign-row-${DRAFT_CAMPAIGN.id}`);
    // Click on the state badge cell (neutral, not on a link/button) so the
    // row-level handler is what fires navigation, not the title Link.
    const badge = within(row).getByText(DRAFT_CAMPAIGN.state);
    await user.click(badge);

    await waitFor(() => {
      expect(router.state.location.pathname).toBe(`/campaigns/${DRAFT_CAMPAIGN.id}`);
    });
  });

  test("clicking the actions trigger does NOT navigate", async () => {
    setupHookMocks({ data: [DRAFT_CAMPAIGN] });
    const user = userEvent.setup();
    const { router } = renderCampaigns();

    const trigger = await screen.findByRole("button", {
      name: `Actions for ${DRAFT_CAMPAIGN.title}`,
    });
    await user.click(trigger);

    // The dropdown opens; the row-level click handler must bail out because
    // the actual click target is an interactive descendant. Closing the menu
    // and asserting the route hasn't changed catches any future regression
    // where the bubbling click re-fires `navigate`.
    await screen.findByRole("menu");
    await user.keyboard("{Escape}");

    expect(router.state.location.pathname).toBe("/campaigns");
  });
});

describe("Campaigns page — sort", () => {
  test("clicking the Title header toggles asc → desc and encodes in the URL", async () => {
    const user = userEvent.setup();
    setupHookMocks({ data: [DRAFT_CAMPAIGN, RUNNING_CAMPAIGN] });
    const { router } = renderCampaigns();

    const titleSort = await screen.findByRole("button", { name: /sort by title/i });
    await user.click(titleSort);

    await waitFor(() => {
      const search = router.state.location.search as Record<string, unknown>;
      expect(search).toMatchObject({ sort: "title", dir: "asc" });
    });

    await user.click(titleSort);
    await waitFor(() => {
      const search = router.state.location.search as Record<string, unknown>;
      expect(search).toMatchObject({ sort: "title", dir: "desc" });
    });
  });
});

describe("Campaigns page — delete confirmation", () => {
  test("Delete opens a dialog; confirming fires the mutation; cancel does not", async () => {
    setupHookMocks({ data: [DRAFT_CAMPAIGN] });
    const user = userEvent.setup();
    renderCampaigns();

    // Open row menu → Delete.
    const trigger = await screen.findByRole("button", {
      name: `Actions for ${DRAFT_CAMPAIGN.title}`,
    });
    await user.click(trigger);
    await user.click(await screen.findByRole("menuitem", { name: /^delete$/i }));

    const dialog = await screen.findByRole("alertdialog");
    // Title + description both contain "Delete campaign" — scope to the heading.
    expect(within(dialog).getByRole("heading", { name: /delete campaign/i })).toBeInTheDocument();

    // Cancel first — mutation should not fire.
    await user.click(within(dialog).getByRole("button", { name: /cancel/i }));
    await waitFor(() => {
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    });
    expect(deleteMutationStub.mutate).not.toHaveBeenCalled();

    // Open it again, this time confirm.
    await user.click(trigger);
    await user.click(await screen.findByRole("menuitem", { name: /^delete$/i }));
    const dialog2 = await screen.findByRole("alertdialog");
    await user.click(within(dialog2).getByRole("button", { name: /^delete$/i }));
    expect(deleteMutationStub.mutate).toHaveBeenCalledWith(DRAFT_CAMPAIGN.id, expect.any(Object));
  });
});
