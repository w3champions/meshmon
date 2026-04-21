import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useState } from "react";
import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  type Campaign,
  useCampaign,
  useDeleteCampaign,
  useEditCampaign,
  usePreviewDispatchCount,
  useStartCampaign,
  useStopCampaign,
} from "@/api/hooks/campaigns";
import { DeleteCampaignDialog } from "@/components/campaigns/DeleteCampaignDialog";
import { EditMetadataSheet } from "@/components/campaigns/EditMetadataSheet";
import { EditPairsSheet } from "@/components/campaigns/EditPairsSheet";
import { CandidatesTab } from "@/components/campaigns/results/CandidatesTab";
import { PairsTab } from "@/components/campaigns/results/PairsTab";
import { RawTab } from "@/components/campaigns/results/RawTab";
import { SettingsTab } from "@/components/campaigns/results/SettingsTab";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { isIllegalStateTransition, stateBadgeVariant } from "@/lib/campaign";
import {
  type CampaignDetailSearch,
  type CampaignDetailTab,
  campaignDetailRoute,
} from "@/router/index";
import { useToastStore } from "@/stores/toast";

// ---------------------------------------------------------------------------
// Static display metadata — kept at module scope so React doesn't reallocate
// every render.
// ---------------------------------------------------------------------------

type PairState = "pending" | "dispatched" | "reused" | "succeeded" | "unreachable" | "skipped";

const PAIR_STATE_ORDER: PairState[] = [
  "pending",
  "dispatched",
  "reused",
  "succeeded",
  "unreachable",
  "skipped",
];

const PAIR_STATE_BADGE_CLASS: Record<PairState, string> = {
  // Muted grey — pending pairs haven't been touched yet.
  pending: "bg-muted text-muted-foreground hover:bg-muted",
  // Blue — dispatched is "in flight", neutral but distinct from success/failure.
  dispatched: "bg-blue-500/15 text-blue-700 dark:text-blue-300 hover:bg-blue-500/20",
  // Cyan — reused is "good but cached" so it reads as success-adjacent.
  reused: "bg-cyan-500/15 text-cyan-700 dark:text-cyan-300 hover:bg-cyan-500/20",
  // Green — succeeded is the happy terminal state.
  succeeded: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300 hover:bg-emerald-500/20",
  // Red — unreachable is the unhappy terminal state.
  unreachable: "bg-destructive/15 text-destructive hover:bg-destructive/20",
  // Amber — skipped is operator-cancelled, not a fault.
  skipped: "bg-amber-500/15 text-amber-700 dark:text-amber-300 hover:bg-amber-500/20",
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Stringify pair counts into a dense `state → count` map, zero-filled for
 * states the server didn't report. The wire shape is `[state, number][]` so a
 * straight `Object.fromEntries` works cleanly.
 */
function pairCountsMap(campaign: Campaign): Record<PairState, number> {
  const seed: Record<PairState, number> = {
    pending: 0,
    dispatched: 0,
    reused: 0,
    succeeded: 0,
    unreachable: 0,
    skipped: 0,
  };
  for (const [state, count] of campaign.pair_counts ?? []) {
    seed[state] = count;
  }
  return seed;
}

// ---------------------------------------------------------------------------
// Row components
// ---------------------------------------------------------------------------

interface StatCardProps {
  label: string;
  value: number | undefined;
  isLoading: boolean;
}

function StatCard({ label, value, isLoading }: StatCardProps) {
  return (
    <Card className="flex flex-col gap-1 p-4">
      <span className="text-xs uppercase tracking-wide text-muted-foreground">{label}</span>
      <span className="text-2xl font-semibold" aria-live="polite">
        {isLoading ? "—" : (value ?? 0).toLocaleString()}
      </span>
    </Card>
  );
}

interface KnobRowProps {
  label: string;
  value: string | number | boolean;
}

function KnobRow({ label, value }: KnobRowProps) {
  // Booleans read as `"false" / "true"` under a plain `String()`, which scans
  // poorly in a knob grid. Render a human label instead; numeric and string
  // values pass through verbatim.
  const display = typeof value === "boolean" ? (value ? "Yes" : "No") : String(value);
  return (
    <div className="flex items-center justify-between gap-3 text-sm">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="font-mono">{display}</dd>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function CampaignDetail() {
  const { id } = campaignDetailRoute.useParams();
  const navigate = useNavigate();
  // `strict: false` keeps the hook usable under the component tests' ad-hoc
  // router tree (where the registered route id differs from production).
  // The `validateSearch` on `campaignDetailRoute` has already coerced the
  // shape at the router boundary, so casting here is safe.
  const search = useSearch({ strict: false }) as CampaignDetailSearch;
  // `tab` is optional in the URL schema so `to: "/campaigns/$id"`
  // navigations don't need to supply one; the page resolves `undefined`
  // to the default landing tab here.
  const tab: CampaignDetailTab = search.tab ?? "candidates";

  // Mount the SSE stream once. `pair_settled` and `state_changed` events
  // invalidate the per-campaign cache keys via the hook's fan-out, so the
  // render below reacts immediately to server-side lifecycle transitions.
  useCampaignStream();

  const campaignQuery = useCampaign(id);
  const previewQuery = usePreviewDispatchCount(id);

  // TanStack Query v5 returns a new result object every render, so listing
  // the whole mutation in a `useCallback` dep array defeats memoization.
  // Destructure `.mutate` — it's a stable reference per mutation. Keep the
  // full `startMutation` / `stopMutation` objects around for the button
  // `isPending` read-outs in the action bar.
  const startMutation = useStartCampaign();
  const stopMutation = useStopCampaign();
  // Restart posts an empty edit body; the server transitions the campaign
  // back to `running` without resetting pair state. Operators who want to
  // re-run every pair reach for "Edit pairs" with force-measurement.
  const editMutation = useEditCampaign();
  const deleteMutation = useDeleteCampaign();
  const { mutate: startCampaign } = startMutation;
  const { mutate: stopCampaign } = stopMutation;
  const { mutate: editCampaign } = editMutation;
  const { mutate: deleteCampaign } = deleteMutation;

  const [editMetadataOpen, setEditMetadataOpen] = useState<boolean>(false);
  const [editPairsOpen, setEditPairsOpen] = useState<boolean>(false);
  const [deleteOpen, setDeleteOpen] = useState<boolean>(false);

  const handleStart = useCallback((): void => {
    startCampaign(id, {
      onError: (err) => {
        const { pushToast } = useToastStore.getState();
        if (isIllegalStateTransition(err)) {
          pushToast({
            kind: "error",
            message: "Can't start — this campaign already advanced.",
          });
          return;
        }
        pushToast({ kind: "error", message: `Start failed: ${err.message}` });
      },
    });
  }, [startCampaign, id]);

  const handleStop = useCallback((): void => {
    stopCampaign(id, {
      onError: (err) => {
        const { pushToast } = useToastStore.getState();
        if (isIllegalStateTransition(err)) {
          pushToast({
            kind: "error",
            message: "Can't stop — this campaign has already finished.",
          });
          return;
        }
        pushToast({ kind: "error", message: `Stop failed: ${err.message}` });
      },
    });
  }, [stopCampaign, id]);

  const handleRestart = useCallback((): void => {
    editCampaign(
      { id, body: {} },
      {
        onError: (err) => {
          const { pushToast } = useToastStore.getState();
          if (isIllegalStateTransition(err)) {
            pushToast({
              kind: "error",
              message: "Can't restart — campaign advanced before the request landed.",
            });
            return;
          }
          pushToast({ kind: "error", message: `Restart failed: ${err.message}` });
        },
      },
    );
  }, [editCampaign, id]);

  const handleConfirmDelete = useCallback(
    (campaignId: string): void => {
      deleteCampaign(campaignId, {
        onSuccess: () => {
          setDeleteOpen(false);
          void navigate({ to: "/campaigns" });
        },
        onError: (err) => {
          // Note: the backend's delete handler (`repo.rs`) is not
          // lifecycle-gated, so we don't branch on 409 here — only the
          // generic fallback. The DeleteCampaignDialog owns its own close
          // path on cancel; leave `deleteOpen` alone so the operator can
          // read the toast and retry without re-opening the dialog.
          const { pushToast } = useToastStore.getState();
          pushToast({
            kind: "error",
            message: `Delete failed: ${err.message}`,
          });
        },
      });
    },
    [deleteCampaign, navigate],
  );

  // -------------------------------------------------------------------------
  // Render branches: loading / 404 / error / happy
  // -------------------------------------------------------------------------

  if (campaignQuery.isLoading) {
    return (
      <main className="flex flex-col gap-4 p-6">
        <div role="status" aria-live="polite" className="flex flex-col gap-3">
          <span className="sr-only">Loading campaign…</span>
          <Skeleton className="h-10 w-1/2" />
          <Skeleton className="h-32 w-full" />
          <Skeleton className="h-24 w-full" />
        </div>
      </main>
    );
  }

  if (campaignQuery.isError) {
    return (
      <main className="flex flex-col gap-3 p-6">
        <h1 className="text-lg font-semibold">Failed to load campaign</h1>
        <p role="alert" className="text-sm text-destructive">
          {campaignQuery.error?.message ?? "Unknown error"}
        </p>
        <div className="flex gap-2">
          <Button onClick={() => campaignQuery.refetch()}>Retry</Button>
          <Button asChild variant="outline">
            <Link to="/campaigns">Back to campaigns</Link>
          </Button>
        </div>
      </main>
    );
  }

  const campaign = campaignQuery.data;
  if (!campaign) {
    return (
      <main className="flex flex-col gap-3 p-6">
        <h1 className="text-lg font-semibold">Campaign not found</h1>
        <p className="text-sm text-muted-foreground">
          The campaign you requested no longer exists.
        </p>
        <Link to="/campaigns" className="text-sm underline underline-offset-2">
          Back to campaigns
        </Link>
      </main>
    );
  }

  const counts = pairCountsMap(campaign);
  const preview = previewQuery.data;
  const { state } = campaign;
  const isTerminal = state === "completed" || state === "stopped" || state === "evaluated";

  return (
    <main className="flex flex-col gap-4 p-6">
      {/* ---------------------------------------------------------------- */}
      {/* Header card — title, notes, state, protocol, timestamps          */}
      {/* ---------------------------------------------------------------- */}
      <Card className="flex flex-col gap-3 p-6">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="flex flex-col gap-1">
            <h1 className="text-2xl font-semibold tracking-tight">{campaign.title}</h1>
            {campaign.notes ? (
              <p className="line-clamp-2 max-w-prose text-sm text-muted-foreground">
                {campaign.notes}
              </p>
            ) : null}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Badge variant={stateBadgeVariant(state)} aria-label={`State: ${state}`}>
              {state}
            </Badge>
            <Badge variant="outline" aria-label={`Protocol: ${campaign.protocol}`}>
              {campaign.protocol.toUpperCase()}
            </Badge>
          </div>
        </div>
        <dl className="grid grid-cols-1 gap-2 text-sm sm:grid-cols-3">
          <div className="flex flex-col">
            <dt className="text-xs uppercase tracking-wide text-muted-foreground">Created</dt>
            <dd className="font-mono">{campaign.created_at}</dd>
          </div>
          <div className="flex flex-col">
            <dt className="text-xs uppercase tracking-wide text-muted-foreground">Started</dt>
            <dd className="font-mono">{campaign.started_at ?? "—"}</dd>
          </div>
          <div className="flex flex-col">
            <dt className="text-xs uppercase tracking-wide text-muted-foreground">Completed</dt>
            <dd className="font-mono">{campaign.completed_at ?? "—"}</dd>
          </div>
        </dl>
      </Card>

      {/* ---------------------------------------------------------------- */}
      {/* Action bar — state-gated per the backend lifecycle machine.       */}
      {/* ---------------------------------------------------------------- */}
      <Card className="flex flex-wrap items-center gap-2 p-4">
        {state === "draft" ? (
          <Button onClick={handleStart} disabled={startMutation.isPending}>
            {startMutation.isPending ? "Starting…" : "Start"}
          </Button>
        ) : null}
        {state === "running" ? (
          <Button onClick={handleStop} variant="destructive" disabled={stopMutation.isPending}>
            {stopMutation.isPending ? "Stopping…" : "Stop"}
          </Button>
        ) : null}
        {isTerminal ? (
          <Button onClick={handleRestart} disabled={editMutation.isPending}>
            {editMutation.isPending ? "Restarting…" : "Restart"}
          </Button>
        ) : null}
        <Button variant="outline" onClick={() => setEditMetadataOpen(true)}>
          Edit metadata
        </Button>
        {isTerminal ? (
          <Button variant="outline" onClick={() => setEditPairsOpen(true)}>
            Edit pairs
          </Button>
        ) : null}
        {state === "draft" || isTerminal ? (
          <Button
            variant="outline"
            className="ml-auto text-destructive hover:text-destructive"
            onClick={() => setDeleteOpen(true)}
          >
            Delete
          </Button>
        ) : null}
      </Card>

      {/* ---------------------------------------------------------------- */}
      {/* Pair-state counts                                                 */}
      {/* ---------------------------------------------------------------- */}
      <Card className="flex flex-col gap-3 p-4">
        <h2 className="text-sm font-semibold">Pair states</h2>
        <ul aria-label="Pair state counts" className="flex list-none flex-wrap gap-2 p-0">
          {PAIR_STATE_ORDER.map((pairState) => {
            const count = counts[pairState];
            return (
              <li key={pairState}>
                <Badge
                  className={PAIR_STATE_BADGE_CLASS[pairState]}
                  aria-label={`${pairState}: ${count}`}
                >
                  {pairState}: {count.toLocaleString()}
                </Badge>
              </li>
            );
          })}
        </ul>
      </Card>

      {/* ---------------------------------------------------------------- */}
      {/* Dispatch preview triple                                          */}
      {/* ---------------------------------------------------------------- */}
      <section aria-label="Dispatch preview" className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <StatCard label="Total" value={preview?.total} isLoading={previewQuery.isLoading} />
        <StatCard label="Reusable" value={preview?.reusable} isLoading={previewQuery.isLoading} />
        <StatCard label="Fresh" value={preview?.fresh} isLoading={previewQuery.isLoading} />
      </section>

      {/* ---------------------------------------------------------------- */}
      {/* Knob read-out                                                    */}
      {/* ---------------------------------------------------------------- */}
      <Card className="flex flex-col gap-3 p-4">
        <h2 className="text-sm font-semibold">Knobs</h2>
        <dl className="grid grid-cols-1 gap-y-2 gap-x-8 sm:grid-cols-2">
          <KnobRow label="Probe count" value={campaign.probe_count} />
          <KnobRow label="Probe count (detail)" value={campaign.probe_count_detail} />
          <KnobRow label="Timeout (ms)" value={campaign.timeout_ms} />
          <KnobRow label="Probe stagger (ms)" value={campaign.probe_stagger_ms} />
          <KnobRow label="Loss threshold (%)" value={campaign.loss_threshold_pct} />
          <KnobRow label="Stddev weight" value={campaign.stddev_weight} />
          <KnobRow label="Evaluation mode" value={campaign.evaluation_mode} />
          <KnobRow label="Force measurement" value={campaign.force_measurement} />
        </dl>
      </Card>

      {/* ---------------------------------------------------------------- */}
      {/* Results tabs                                                     */}
      {/* Radix `TabsContent` keeps every panel in the DOM by default —     */}
      {/* we gate the children on `tab` so only the active sub-component    */}
      {/* mounts. That preserves "lazy tabs" (expensive per-tab queries     */}
      {/* only fire once the operator opens the tab).                       */}
      {/* ---------------------------------------------------------------- */}
      <Tabs
        value={tab}
        onValueChange={(next) => {
          // `useNavigate` without a `to` infers the active route's search
          // shape, but TanStack Router's generic inference here resolves to
          // `never` under the component-test router harness. Cast to the
          // narrow search-only shape so prod and the test harness both
          // type-check. The router's `validateSearch` runs regardless.
          const navigateSearch = navigate as unknown as (opts: {
            search: CampaignDetailSearch;
            replace: boolean;
          }) => void;
          navigateSearch({
            search: { ...search, tab: next as CampaignDetailTab },
            replace: true,
          });
        }}
      >
        <TabsList aria-label="Campaign results tabs">
          <TabsTrigger value="candidates">Candidates</TabsTrigger>
          <TabsTrigger value="pairs">Pairs</TabsTrigger>
          <TabsTrigger value="raw">Raw measurements</TabsTrigger>
          <TabsTrigger value="settings">Evaluation settings</TabsTrigger>
        </TabsList>
        <TabsContent value="candidates">
          {tab === "candidates" ? <CandidatesTab campaign={campaign} /> : null}
        </TabsContent>
        <TabsContent value="pairs">
          {tab === "pairs" ? <PairsTab campaign={campaign} /> : null}
        </TabsContent>
        <TabsContent value="raw">
          {tab === "raw" ? <RawTab campaign={campaign} /> : null}
        </TabsContent>
        <TabsContent value="settings">
          {tab === "settings" ? <SettingsTab campaign={campaign} /> : null}
        </TabsContent>
      </Tabs>

      {/* ---------------------------------------------------------------- */}
      {/* Sheets + dialogs                                                 */}
      {/* ---------------------------------------------------------------- */}
      <EditMetadataSheet
        campaign={campaign}
        open={editMetadataOpen}
        onOpenChange={setEditMetadataOpen}
      />
      <EditPairsSheet campaign={campaign} open={editPairsOpen} onOpenChange={setEditPairsOpen} />
      <DeleteCampaignDialog
        campaign={campaign}
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        onConfirm={handleConfirmDelete}
        isPending={deleteMutation.isPending}
      />
    </main>
  );
}
