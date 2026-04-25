import { useNavigate } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useAgents } from "@/api/hooks/agents";
import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  type CreateCampaignBody,
  type ProbeProtocol,
  useCreateCampaign,
  useDeleteCampaign,
  usePreviewDispatchCount,
  useStartCampaign,
} from "@/api/hooks/campaigns";
import { useCatalogueFacets, useCatalogueMap } from "@/api/hooks/catalogue";
import { DestinationPanel } from "@/components/campaigns/DestinationPanel";
import { KnobPanel } from "@/components/campaigns/KnobPanel";
import { SizePreview } from "@/components/campaigns/SizePreview";
import { SourcePanel } from "@/components/campaigns/SourcePanel";
import { StartConfirmDialog } from "@/components/campaigns/StartConfirmDialog";
import type { FilterValue } from "@/components/filter/FilterRail";
import { DrawMap, type DrawMapPin } from "@/components/map/DrawMap";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { extractCampaignErrorCode } from "@/lib/campaign";
import { type CampaignKnobs, DEFAULT_KNOBS, SIZE_WARNING_THRESHOLD } from "@/lib/campaign-config";
import { destinationFilterToQuery } from "@/lib/catalogue-query";
import type { Bbox } from "@/lib/geo";
import { useComposerSeedStore } from "@/stores/composer-seed";
import { useToastStore } from "@/stores/toast";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_MAP_ZOOM = 2;

const EMPTY_FILTER: FilterValue = {
  countryCodes: [],
  asns: [],
  networks: [],
  cities: [],
  shapes: [],
};

type MapTarget = "source" | "dest" | null;

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function CampaignComposer() {
  // Mount the SSE stream once per composer mount. Mirrors Campaigns.tsx:
  // the stream invalidates campaign caches on every lifecycle event, so
  // the composer sees the draft it just created (and any concurrent
  // operator edits) without waiting for polling.
  useCampaignStream();

  const navigate = useNavigate();
  const facetsQuery = useCatalogueFacets();

  // --- Draft state -------------------------------------------------------
  const [sourceFilter, setSourceFilter] = useState<FilterValue>(EMPTY_FILTER);
  const [destFilter, setDestFilter] = useState<FilterValue>(EMPTY_FILTER);
  const [sourceSet, setSourceSet] = useState<Set<string>>(new Set());
  const [destSet, setDestSet] = useState<Set<string>>(new Set());
  const [knobs, setKnobs] = useState<CampaignKnobs>(DEFAULT_KNOBS);

  // --- Composer-seed hand-off --------------------------------------------
  // A Clone click on a terminal campaign stashes a fully-resolved knob
  // snapshot + deduped source/destination sets into the transient
  // `composer-seed` store and navigates here. We consume the seed exactly
  // once on mount and hydrate local state from it. A plain open of
  // `/campaigns/new` (no prior Clone) sees `null` and keeps defaults.
  const consumeSeed = useComposerSeedStore((s) => s.consumeSeed);
  useEffect(() => {
    const seed = consumeSeed();
    if (!seed) return;
    setKnobs(seed.knobs);
    setSourceSet(new Set(seed.sourceSet));
    setDestSet(new Set(seed.destSet));
    // `consumeSeed` is stable (Zustand selector), so React's rules-of-hooks
    // lint passes with it in the dep array even though the effect is
    // mount-only by design.
  }, [consumeSeed]);

  // --- Map dialog state --------------------------------------------------
  const [mapOpenFor, setMapOpenFor] = useState<MapTarget>(null);
  const [mapViewport, setMapViewport] = useState<{ bbox: Bbox; zoom: number } | null>(null);

  // --- Dispatch flow state ----------------------------------------------
  const [draftCampaignId, setDraftCampaignId] = useState<string | null>(null);
  const [confirmDialogOpen, setConfirmDialogOpen] = useState(false);
  const [autoStartAfterCreate, setAutoStartAfterCreate] = useState(false);

  // --- Hooks --------------------------------------------------------------
  const createMutation = useCreateCampaign();
  const startMutation = useStartCampaign();
  const deleteMutation = useDeleteCampaign();
  const previewQuery = usePreviewDispatchCount(draftCampaignId ?? undefined);

  // Once a draft is created, inputs lock: the create payload has already
  // been persisted server-side, so further edits would be silently dropped
  // at Start time. The operator must Back out (which deletes the draft)
  // and start a new composer if they need to change anything.
  const draftLocked = draftCampaignId !== null;

  // --- Map pins (gated on which panel opened the dialog) ----------------

  const agentsQuery = useAgents();
  const sourcePins: DrawMapPin[] = useMemo(() => {
    if (mapOpenFor !== "source") return [];
    const out: DrawMapPin[] = [];
    for (const a of agentsQuery.data ?? []) {
      const coords = a.catalogue_coordinates;
      if (coords == null) continue;
      out.push({ id: a.id, lat: coords.latitude, lon: coords.longitude });
    }
    return out;
  }, [mapOpenFor, agentsQuery.data]);

  const destMapQuery = useMemo(() => {
    // The map endpoint does not carry `shapes` or `city` — mirror
    // Catalogue.tsx. Those two keys stay out of the projection below.
    const q = destinationFilterToQuery(destFilter);
    const { shapes: _shapes, city: _city, ...rest } = q;
    return rest;
  }, [destFilter]);

  const destMapResp = useCatalogueMap(
    mapViewport?.bbox,
    mapViewport?.zoom ?? DEFAULT_MAP_ZOOM,
    destMapQuery,
    { enabled: mapOpenFor === "dest" },
  );

  const destPins: DrawMapPin[] = useMemo(() => {
    if (mapOpenFor !== "dest") return [];
    const resp = destMapResp.data;
    if (!resp) return [];
    if (resp.kind === "detail") {
      return resp.rows
        .filter((e) => e.latitude != null && e.longitude != null)
        .map((e) => ({
          id: e.id,
          lat: e.latitude as number,
          lon: e.longitude as number,
        }));
    }
    return resp.buckets.map((b) => ({
      id: `cluster-${b.sample_id}`,
      lat: b.lat,
      lon: b.lng,
    }));
  }, [mapOpenFor, destMapResp.data]);

  const handleViewportChange = useCallback((bbox: Bbox, zoom: number) => {
    setMapViewport({ bbox, zoom });
  }, []);

  // --- Start flow --------------------------------------------------------

  const focusTitle = useCallback(() => {
    const el = document.getElementById("campaign-title");
    if (el instanceof HTMLInputElement) el.focus();
  }, []);

  const buildCreateBody = useCallback((): CreateCampaignBody => {
    // `knobs.protocol` can still be the UI-only "mtr" sentinel, but the
    // validation gate in `handleStart` blocks the flow before this helper
    // runs. Cast is safe by contract.
    return {
      title: knobs.title,
      notes: knobs.notes || undefined,
      protocol: knobs.protocol as ProbeProtocol,
      probe_count: knobs.probe_count,
      probe_count_detail: knobs.probe_count_detail,
      timeout_ms: knobs.timeout_ms,
      probe_stagger_ms: knobs.probe_stagger_ms,
      loss_threshold_ratio: knobs.loss_threshold_ratio,
      stddev_weight: knobs.stddev_weight,
      evaluation_mode: knobs.evaluation_mode,
      // Guardrail knobs are nullable on the wire — `null` means "gate
      // disabled" on CREATE (the backend leaves the column NULL).
      max_transit_rtt_ms: knobs.max_transit_rtt_ms,
      max_transit_stddev_ms: knobs.max_transit_stddev_ms,
      min_improvement_ms: knobs.min_improvement_ms,
      min_improvement_ratio: knobs.min_improvement_ratio,
      force_measurement: knobs.force_measurement,
      source_agent_ids: Array.from(sourceSet),
      destination_ips: Array.from(destSet),
    };
  }, [knobs, sourceSet, destSet]);

  const runStart = useCallback(
    (campaignId: string): void => {
      startMutation.mutate(campaignId, {
        onSuccess: () => {
          void navigate({
            to: "/campaigns/$id",
            params: { id: campaignId },
          });
        },
        onError: (err) => {
          const code = extractCampaignErrorCode(err);
          const { pushToast } = useToastStore.getState();
          if (code === "illegal_state_transition") {
            pushToast({
              kind: "error",
              message: "Campaign already running or finalized.",
            });
            return;
          }
          pushToast({ kind: "error", message: `Start failed: ${err.message}` });
        },
      });
    },
    [startMutation, navigate],
  );

  const handleStart = useCallback(
    (event?: React.FormEvent<HTMLFormElement>) => {
      if (event) event.preventDefault();

      const { pushToast } = useToastStore.getState();

      // --- Client-side validation gate (defense-in-depth) ---------------
      if (knobs.title.trim() === "") {
        pushToast({ kind: "error", message: "Title is required." });
        focusTitle();
        return;
      }
      if (sourceSet.size === 0) {
        pushToast({ kind: "error", message: "Select at least one source." });
        return;
      }
      if (destSet.size === 0) {
        pushToast({
          kind: "error",
          message: "Select at least one destination.",
        });
        return;
      }
      if (knobs.protocol === "mtr") {
        // The UI already keeps the Start button disabled here; defense in
        // depth in case the form is submitted via Enter on the title.
        pushToast({
          kind: "error",
          message: "MTR is not a valid campaign protocol.",
        });
        return;
      }

      // --- Phase 2: already created → decide whether to confirm or fire --
      if (draftCampaignId !== null) {
        // Fail-safe: the preview may be undefined if the query errored or
        // is mid-refetch. Defaulting `fresh` to 0 here would silently
        // bypass the threshold gate on a second click, so require the
        // operator to retry once the preview lands.
        if (!previewQuery.data) {
          pushToast({
            kind: "error",
            message: "Preview not available yet — please retry.",
          });
          return;
        }
        const fresh = previewQuery.data.fresh;
        if (fresh > SIZE_WARNING_THRESHOLD) {
          setConfirmDialogOpen(true);
          return;
        }
        runStart(draftCampaignId);
        return;
      }

      // --- Phase 1: create the draft, then let the preview gate take over -
      setAutoStartAfterCreate(true);
      createMutation.mutate(buildCreateBody(), {
        onSuccess: (created) => {
          setDraftCampaignId(created.id);
          // `SizePreview` takes over from here — it polls the preview
          // endpoint and fires `onThresholdExceeded` once. If it doesn't
          // cross the threshold, the effect in `runIfReadyToStart` will
          // fire as soon as the preview resolves.
        },
        onError: (err) => {
          setAutoStartAfterCreate(false);
          const code = extractCampaignErrorCode(err);
          if (code === "title_required") {
            pushToast({ kind: "error", message: "Title is required." });
            focusTitle();
            return;
          }
          if (code === "invalid_destination_ip") {
            pushToast({
              kind: "error",
              message: "One or more destinations were rejected by the server.",
            });
            return;
          }
          pushToast({
            kind: "error",
            message: `Create failed: ${err.message}`,
          });
        },
      });
    },
    [
      knobs,
      sourceSet,
      destSet,
      draftCampaignId,
      previewQuery.data,
      createMutation,
      buildCreateBody,
      focusTitle,
      runStart,
    ],
  );

  // Once the preview resolves after a `handleStart` create, auto-proceed
  // if we're under threshold. The `autoStartAfterCreate` latch is set at
  // Start-click time and cleared here so the auto-proceed is one-shot
  // per create cycle.
  useEffect(() => {
    if (!autoStartAfterCreate) return;
    if (draftCampaignId === null) return;
    if (previewQuery.data === undefined) return;
    const fresh = previewQuery.data.fresh;
    setAutoStartAfterCreate(false);
    if (fresh > SIZE_WARNING_THRESHOLD) {
      setConfirmDialogOpen(true);
    } else {
      runStart(draftCampaignId);
    }
  }, [autoStartAfterCreate, draftCampaignId, previewQuery.data, runStart]);

  const handleConfirmStart = useCallback(() => {
    if (draftCampaignId === null) return;
    setConfirmDialogOpen(false);
    runStart(draftCampaignId);
  }, [draftCampaignId, runStart]);

  // Back discards the draft. When a `draftCampaignId` exists the create
  // already hit the server — navigate-away-without-delete would leave an
  // orphan draft row that the operator can't see from this screen. Fire
  // the delete mutation on the way out; navigation waits for the delete
  // to settle so a fast-click-retry doesn't race the still-present draft.
  const { mutate: deleteCampaignMutate } = deleteMutation;
  const handleBack = useCallback(() => {
    if (draftCampaignId === null) {
      void navigate({ to: "/campaigns" });
      return;
    }
    deleteCampaignMutate(draftCampaignId, {
      onError: (err) => {
        // Surface a toast so the operator sees that the draft may still
        // exist — the mutation's error state is lost the moment we
        // navigate away, and silently abandoning a failed delete risks an
        // orphan draft the operator never learns about.
        useToastStore.getState().pushToast({
          kind: "error",
          message: `Draft may still exist — check the list. ${err.message}`,
        });
      },
      onSettled: () => {
        // Regardless of outcome — a 404 means the draft's already gone,
        // other errors surface via the toast above but shouldn't trap the
        // operator on this page (they explicitly asked to go back).
        setDraftCampaignId(null);
        void navigate({ to: "/campaigns" });
      },
    });
  }, [draftCampaignId, deleteCampaignMutate, navigate]);

  // Note: SizePreview's `onThresholdExceeded` would also fire when fresh
  // crosses the threshold, but we intentionally do not wire it here — the
  // `autoStartAfterCreate` effect above owns the one-shot "open the
  // confirm dialog once after create" behavior. If the operator cancels
  // and the preview refetches a still-over-threshold count, we don't
  // want SizePreview silently re-opening the dialog.

  // --- Map dialog helpers -----------------------------------------------

  const openSourceMap = useCallback(() => setMapOpenFor("source"), []);
  const openDestMap = useCallback(() => setMapOpenFor("dest"), []);
  const closeMapDialog = useCallback(() => {
    setMapOpenFor(null);
    setMapViewport(null);
  }, []);

  const activeFilter = mapOpenFor === "source" ? sourceFilter : destFilter;
  const setActiveFilter = mapOpenFor === "source" ? setSourceFilter : setDestFilter;
  const activePins = mapOpenFor === "source" ? sourcePins : destPins;

  // --- Computed UI state --------------------------------------------------

  const startDisabled =
    knobs.protocol === "mtr" || createMutation.isPending || startMutation.isPending;

  // Pre-commit view: always mirror the operator's explicit selection. The
  // DestinationPanel footer already advertises the filter's first-page total
  // ("N of M matching"), so the preview doesn't double as a filter hint —
  // that fallback made Remove-all visibly no-op because the preview kept
  // showing the full filter total.
  const approxDestTotal = destSet.size;

  return (
    <form onSubmit={handleStart} aria-label="Create campaign" className="flex flex-col gap-4">
      <header className="flex flex-wrap items-baseline justify-between gap-3">
        <h1 className="text-2xl font-semibold">Create campaign</h1>
      </header>

      {draftLocked ? (
        <div
          role="status"
          aria-live="polite"
          className="rounded-md border border-primary/30 bg-primary/5 px-3 py-2 text-sm"
        >
          Draft created — further edits require starting a new campaign. Use Back to discard this
          draft and restart.
        </div>
      ) : null}

      <div className="grid grid-cols-1 gap-4 md:grid-cols-[minmax(0,2fr)_minmax(320px,1fr)]">
        {/* Left column — source + destination pickers */}
        <div className="flex flex-col gap-4">
          <Card className="flex flex-col gap-3 p-4">
            <SourcePanel
              selected={sourceSet}
              onSelectedChange={setSourceSet}
              filter={sourceFilter}
              onFilterChange={setSourceFilter}
              facets={facetsQuery.data}
              onOpenMap={openSourceMap}
              disabled={draftLocked}
            />
          </Card>
          <Card className="flex flex-col gap-3 p-4">
            <DestinationPanel
              selected={destSet}
              onSelectedChange={setDestSet}
              filter={destFilter}
              onFilterChange={setDestFilter}
              facets={facetsQuery.data}
              onOpenMap={openDestMap}
              disabled={draftLocked}
            />
          </Card>
        </div>

        {/* Right column — knobs, preview, action bar */}
        <div className="flex flex-col gap-4">
          <Card className="p-4">
            <KnobPanel value={knobs} onChange={setKnobs} disabled={draftLocked} />
          </Card>
          <Card className="p-4">
            {/* onThresholdExceeded intentionally unset — composer owns threshold
                gating via the autoStartAfterCreate effect. See block comment
                above runStart. */}
            <SizePreview
              sourcesSelected={sourceSet.size}
              approxTotal={approxDestTotal}
              shapesActive={destFilter.shapes.length > 0}
              campaignId={draftCampaignId ?? undefined}
              forceMeasurement={knobs.force_measurement}
              sizeWarningThreshold={SIZE_WARNING_THRESHOLD}
            />
          </Card>
          <div className="flex items-center justify-end gap-2">
            <Button
              type="button"
              variant="outline"
              onClick={handleBack}
              disabled={deleteMutation.isPending}
            >
              Back
            </Button>
            <Button type="submit" disabled={startDisabled} aria-disabled={startDisabled}>
              {createMutation.isPending || startMutation.isPending ? "Starting…" : "Start"}
            </Button>
          </div>
        </div>
      </div>

      <Dialog
        open={mapOpenFor !== null}
        onOpenChange={(next) => {
          if (!next) closeMapDialog();
        }}
      >
        <DialogContent className="max-w-4xl">
          <DialogHeader>
            <DialogTitle>{mapOpenFor === "source" ? "Source map" : "Destination map"}</DialogTitle>
          </DialogHeader>
          {mapOpenFor !== null ? (
            <DrawMap
              shapes={activeFilter.shapes}
              onShapesChange={(shapes) => setActiveFilter({ ...activeFilter, shapes })}
              pins={activePins}
              onViewportChange={mapOpenFor === "dest" ? handleViewportChange : undefined}
              className="h-[60vh]"
            />
          ) : null}
        </DialogContent>
      </Dialog>

      <StartConfirmDialog
        open={confirmDialogOpen}
        onOpenChange={setConfirmDialogOpen}
        freshCount={previewQuery.data?.fresh ?? 0}
        onConfirm={handleConfirmStart}
        isStarting={startMutation.isPending}
      />
    </form>
  );
}
