import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { useCallback, useState } from "react";
import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  type Campaign,
  useCampaign,
  useCampaignPairs,
  useDeleteCampaign,
  useEditCampaign,
  usePreviewDispatchCount,
  useStartCampaign,
  useStopCampaign,
} from "@/api/hooks/campaigns";
import { useEvaluation } from "@/api/hooks/evaluation";
import { DeleteCampaignDialog } from "@/components/campaigns/DeleteCampaignDialog";
import { EditMetadataSheet } from "@/components/campaigns/EditMetadataSheet";
import { CandidatesTab } from "@/components/campaigns/results/CandidatesTab";
import { CompareTab } from "@/components/campaigns/results/CompareTab";
import { HeatmapTab } from "@/components/campaigns/results/HeatmapTab";
import { PairsTab } from "@/components/campaigns/results/PairsTab";
import { RawTab } from "@/components/campaigns/results/RawTab";
import { SettingsTab } from "@/components/campaigns/results/SettingsTab";
import { CatalogueDrawerOverlay } from "@/components/catalogue/CatalogueDrawerOverlay";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { isIllegalStateTransition, stateBadgeVariant } from "@/lib/campaign";
import { ratioToPercentInput } from "@/lib/campaign-config";
import {
  type CampaignDetailSearch,
  type CampaignDetailTab,
  campaignDetailRoute,
} from "@/router/index";
import { useComposerSeedStore } from "@/stores/composer-seed";
import { useToastStore } from "@/stores/toast";

// ---------------------------------------------------------------------------
// Static display metadata — kept at module scope so React doesn't reallocate
// every render.
// ---------------------------------------------------------------------------

/**
 * Upper bound on pairs the Clone action ferries into the composer seed.
 * Matches the backend `GET /api/campaigns/:id/pairs` handler-side clamp
 * (`crates/service/src/campaign/handlers.rs::pairs`). Beyond this cap
 * the Clone toast warns the operator before handing off.
 */
const CLONE_PAIR_CAP = 5000;

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
  /**
   * `null` is rendered as the em-dash "off" sentinel — used for the
   * optional guardrail knobs (`max_transit_rtt_ms`, etc.) where an
   * unset column means "gate disabled".
   */
  value: string | number | boolean | null;
}

function KnobRow({ label, value }: KnobRowProps) {
  // Booleans read as `"false" / "true"` under a plain `String()`, which scans
  // poorly in a knob grid. Render a human label instead; `null` collapses to
  // an em-dash; numeric and string values pass through verbatim.
  let display: string;
  if (value === null) {
    display = "—";
  } else if (typeof value === "boolean") {
    display = value ? "Yes" : "No";
  } else {
    display = String(value);
  }
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
  // `tab` is `.optional()` on the URL schema so navigations to
  // `/campaigns/$id` without a search clause still type-check, but the
  // router's validator (`parseCampaignDetailSearch`) always fills
  // `tab = "candidates"` when absent and catches invalid values. The
  // `?? "candidates"` here is a type-level narrowing backstop — at runtime
  // `search.tab` is guaranteed populated by the validator.
  const tab: CampaignDetailTab = search.tab ?? "candidates";

  // Mount the SSE stream once. `pair_settled` and `state_changed` events
  // invalidate the per-campaign cache keys via the hook's fan-out, so the
  // render below reacts immediately to server-side lifecycle transitions.
  useCampaignStream();

  const campaignQuery = useCampaign(id);
  const previewQuery = usePreviewDispatchCount(id);
  const evaluationQuery = useEvaluation(id);

  // TanStack Query v5 returns a new result object every render, so listing
  // the whole mutation in a `useCallback` dep array defeats memoization.
  // Destructure `.mutate` — it's a stable reference per mutation. Keep the
  // full `startMutation` / `stopMutation` objects around for the button
  // `isPending` read-outs in the action bar.
  const startMutation = useStartCampaign();
  const stopMutation = useStopCampaign();
  // Restart posts an empty edit body; the server transitions the campaign
  // back to `running` without resetting pair state. Operators who want to
  // re-run the whole campaign with tweaked knobs reach for Clone, which
  // seeds the composer with the source/destination sets and starts a
  // fresh draft.
  const editMutation = useEditCampaign();
  const deleteMutation = useDeleteCampaign();
  const { mutate: startCampaign } = startMutation;
  const { mutate: stopCampaign } = stopMutation;
  const { mutate: editCampaign } = editMutation;
  const { mutate: deleteCampaign } = deleteMutation;

  // Pull the full pair list for the Clone action — gated on the campaign
  // loading into a terminal state. `useCampaignPairs` is disabled when
  // `id` is undefined, so passing `undefined` for non-terminal campaigns
  // keeps the query dormant without breaking the rules of hooks. `limit`
  // matches the backend's own `list_pairs` cap so the seed carries every
  // pair the handler is willing to return; truncation detection uses
  // `pair_counts` against the received page length rather than the
  // request limit.
  const terminalState =
    campaignQuery.data?.state === "completed" ||
    campaignQuery.data?.state === "stopped" ||
    campaignQuery.data?.state === "evaluated";
  const pairsQuery = useCampaignPairs(terminalState ? id : undefined, {
    limit: CLONE_PAIR_CAP,
  });

  const setComposerSeed = useComposerSeedStore((s) => s.setSeed);

  const [editMetadataOpen, setEditMetadataOpen] = useState<boolean>(false);
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

  // Clone — seed the composer from this terminal campaign's knobs +
  // deduped source/destination sets, then navigate to `/campaigns/new`.
  // The composer consumes the seed exactly once on mount; a fresh load
  // of `/campaigns/new` without a prior Clone starts from defaults.
  //
  // `pairsLoaded` gates on a non-empty array — an empty list is not a
  // useful seed (no sources, no destinations) so the button stays
  // disabled even though the query technically resolved. The
  // `handleClone` guard mirrors this so a concurrent re-render that
  // flips `pairsData` back to empty can't slip a zero-pair seed
  // through into the composer store.
  const pairsData = pairsQuery.data;
  const pairsLoaded = Boolean(pairsData?.length);
  const cloneCampaign = campaignQuery.data ?? null;
  // Derive the Clone button's label + click behavior from the pairs
  // query state so the four reachable states (loading, error, empty,
  // ready) each map to a distinct affordance. Mirrors the Start/Stop
  // `isPending ? "…" : "…"` idiom used elsewhere in the action bar.
  const cloneButtonState: "loading" | "error" | "ready" = pairsQuery.isLoading
    ? "loading"
    : pairsQuery.isError
      ? "error"
      : "ready";
  const cloneLabel =
    cloneButtonState === "loading"
      ? "Clone (loading…)"
      : cloneButtonState === "error"
        ? "Clone (retry)"
        : "Clone";
  const handleClone = useCallback((): void => {
    if (!cloneCampaign || !pairsData?.length) return;
    // `pair_counts` is the authoritative total of baseline
    // (`kind='campaign'`) pairs — same filter the `list_pairs` handler
    // applies — so comparing it against the received page length
    // detects truncation without the `>= CAP` heuristic's false
    // positive at exactly `CAP`. List-view responses don't populate
    // `pair_counts` (see the OpenAPI schema); the single-row
    // `GET /api/campaigns/:id` this page uses always does.
    const totalBaselinePairs =
      cloneCampaign.pair_counts?.reduce((sum, [, n]) => sum + n, 0) ?? pairsData.length;
    if (totalBaselinePairs > pairsData.length) {
      useToastStore.getState().pushToast({
        kind: "error",
        message: `This campaign has ${totalBaselinePairs.toLocaleString()} pairs; Clone truncated the seed to the first ${pairsData.length.toLocaleString()}. Review before starting.`,
      });
    }
    const sourceSet = [...new Set(pairsData.map((p) => p.source_agent_id))];
    const destSet = [...new Set(pairsData.map((p) => p.destination_ip))];
    setComposerSeed({
      knobs: {
        title: `Copy of ${cloneCampaign.title}`,
        notes: cloneCampaign.notes,
        protocol: cloneCampaign.protocol,
        probe_count: cloneCampaign.probe_count,
        probe_count_detail: cloneCampaign.probe_count_detail,
        timeout_ms: cloneCampaign.timeout_ms,
        probe_stagger_ms: cloneCampaign.probe_stagger_ms,
        loss_threshold_ratio: cloneCampaign.loss_threshold_ratio,
        stddev_weight: cloneCampaign.stddev_weight,
        evaluation_mode: cloneCampaign.evaluation_mode,
        // Carry guardrail knobs forward — Clone preserves the original
        // operator intent. `?? null` collapses `undefined` (older campaign
        // rows where the field never landed) to `null`.
        max_transit_rtt_ms: cloneCampaign.max_transit_rtt_ms ?? null,
        max_transit_stddev_ms: cloneCampaign.max_transit_stddev_ms ?? null,
        min_improvement_ms: cloneCampaign.min_improvement_ms ?? null,
        min_improvement_ratio: cloneCampaign.min_improvement_ratio ?? null,
        // Reset `force_measurement` — clones default to reuse-cache
        // friendly so a tweak-and-rerun doesn't silently re-measure
        // every pair. Operator opts in via the knob panel.
        force_measurement: false,
        // Carry the edge-candidate knobs forward. `?? default` collapses
        // `undefined` on older campaign rows to sensible defaults.
        useful_latency_ms: cloneCampaign.useful_latency_ms ?? null,
        max_hops: cloneCampaign.max_hops ?? 2,
        vm_lookback_minutes: cloneCampaign.vm_lookback_minutes ?? 15,
      },
      sourceSet,
      destSet,
    });
    void navigate({ to: "/campaigns/new" });
  }, [cloneCampaign, pairsData, setComposerSeed, navigate]);

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

  // Guard tabs that are conditionally available: fall back to "candidates"
  // when the URL requests a tab that is hidden for this campaign's mode/state.
  const isEdgeCandidate = campaign.evaluation_mode === "edge_candidate";
  const effectiveTab: CampaignDetailTab =
    (tab === "heatmap" && !isEdgeCandidate) || (tab === "compare" && !isTerminal)
      ? "candidates"
      : tab;

  // A `campaign_evaluations` row survives knob-change dismissal — only the
  // campaign row's `state` flips back to `completed` and `evaluated_at`
  // clears. `GET /evaluation` therefore keeps returning the historical
  // snapshot. Render the evaluation-derived tabs only when the snapshot
  // is current (state is `evaluated`) AND the snapshot's mode matches the
  // campaign's current mode (operator may have PATCHed the mode against a
  // historical row that targeted a different mode). Tabs receive `null`
  // when the snapshot is stale so their existing null-gate falls through
  // to the placeholder.
  const evaluation = evaluationQuery.data ?? null;
  const hasFreshEvaluation =
    state === "evaluated" && evaluation?.evaluation_mode === campaign.evaluation_mode;
  const freshEvaluation = hasFreshEvaluation ? evaluation : null;

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
          <Button
            variant="outline"
            // "error" → click refetches; "ready" → click seeds +
            // navigates; "loading" is disabled so no onClick path.
            onClick={cloneButtonState === "error" ? () => pairsQuery.refetch() : handleClone}
            // Disabled whenever the pair list can't seed the composer:
            // still loading, failed to load, or resolved but empty.
            disabled={cloneButtonState !== "error" && !pairsLoaded}
            aria-label="Clone campaign"
            title={
              cloneButtonState === "error" ? "Failed to load pairs — click to retry" : undefined
            }
          >
            {cloneLabel}
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
          <KnobRow
            label="Loss threshold (%)"
            value={ratioToPercentInput(campaign.loss_threshold_ratio)}
          />
          <KnobRow label="Stddev weight" value={campaign.stddev_weight} />
          <KnobRow label="Evaluation mode" value={campaign.evaluation_mode} />
          <KnobRow label="Force measurement" value={campaign.force_measurement} />
          {/*
           * Cross-mode knobs that drive the evaluator. `max_hops` and
           * `vm_lookback_minutes` apply to every mode; `useful_latency_ms`
           * is only consumed by edge_candidate, so we only surface it
           * when the campaign opted into that mode. Labels match the
           * SettingsTab inputs verbatim for at-a-glance parity.
           */}
          {isEdgeCandidate ? (
            <KnobRow label="Useful latency (ms)" value={campaign.useful_latency_ms ?? null} />
          ) : null}
          <KnobRow label="Max hops" value={campaign.max_hops ?? null} />
          <KnobRow label="Lookback window (min)" value={campaign.vm_lookback_minutes ?? null} />
          {/*
           * Guardrail knobs. Optional on the campaign row — `?? null`
           * collapses `undefined` (legacy campaigns predating the
           * columns) to the em-dash sentinel rendered by `KnobRow`.
           * Labels match the SettingsTab inputs verbatim for at-a-
           * glance parity.
           */}
          <KnobRow label="Max transit RTT (ms)" value={campaign.max_transit_rtt_ms ?? null} />
          <KnobRow label="Max transit stddev (ms)" value={campaign.max_transit_stddev_ms ?? null} />
          <KnobRow label="Min improvement (ms)" value={campaign.min_improvement_ms ?? null} />
          <KnobRow label="Min improvement ratio" value={campaign.min_improvement_ratio ?? null} />
        </dl>
      </Card>

      {/* ---------------------------------------------------------------- */}
      {/* Results tabs                                                     */}
      {/* Radix `TabsContent` keeps every panel in the DOM by default —     */}
      {/* we gate the children on `tab` so only the active sub-component    */}
      {/* mounts. That preserves "lazy tabs" (expensive per-tab queries     */}
      {/* only fire once the operator opens the tab).                       */}
      {/* ---------------------------------------------------------------- */}
      <CatalogueDrawerOverlay>
        <Tabs
          value={effectiveTab}
          // Spreading `search` preserves `raw_*` filter params across tab
          // switches by design — the operator keeps their filter selection
          // when navigating between Raw and other tabs. If they open Raw,
          // apply a `raw_state=pending` filter, then visit Settings, the
          // filter survives the round-trip back.
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
            {isEdgeCandidate ? <TabsTrigger value="heatmap">Heatmap</TabsTrigger> : null}
            <TabsTrigger value="pairs">Pairs</TabsTrigger>
            {isTerminal ? <TabsTrigger value="compare">Compare</TabsTrigger> : null}
            <TabsTrigger value="raw">Raw measurements</TabsTrigger>
            <TabsTrigger value="settings">Evaluation settings</TabsTrigger>
          </TabsList>
          <TabsContent value="candidates">
            {effectiveTab === "candidates" ? (
              <CandidatesTab campaign={campaign} freshEvaluation={freshEvaluation} />
            ) : null}
          </TabsContent>
          {isEdgeCandidate ? (
            <TabsContent value="heatmap">
              {effectiveTab === "heatmap" && freshEvaluation ? (
                <HeatmapTab campaign={campaign} evaluation={freshEvaluation} />
              ) : effectiveTab === "heatmap" ? (
                <p className="text-sm text-muted-foreground p-4">
                  Evaluate first to view the heatmap.
                </p>
              ) : null}
            </TabsContent>
          ) : null}
          <TabsContent value="pairs">
            {effectiveTab === "pairs" ? (
              <PairsTab campaign={campaign} evaluation={freshEvaluation} />
            ) : null}
          </TabsContent>
          {isTerminal ? (
            <TabsContent value="compare">
              {effectiveTab === "compare" ? (
                <CompareTab campaign={campaign} evaluation={freshEvaluation} />
              ) : null}
            </TabsContent>
          ) : null}
          <TabsContent value="raw">
            {effectiveTab === "raw" ? <RawTab campaign={campaign} /> : null}
          </TabsContent>
          <TabsContent value="settings">
            {effectiveTab === "settings" ? <SettingsTab campaign={campaign} /> : null}
          </TabsContent>
        </Tabs>
      </CatalogueDrawerOverlay>

      {/* ---------------------------------------------------------------- */}
      {/* Sheets + dialogs                                                 */}
      {/* ---------------------------------------------------------------- */}
      <EditMetadataSheet
        campaign={campaign}
        open={editMetadataOpen}
        onOpenChange={setEditMetadataOpen}
      />
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
