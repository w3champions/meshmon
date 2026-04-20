import { useNavigate } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useAgents } from "@/api/hooks/agents";
import { useCampaignStream } from "@/api/hooks/campaign-stream";
import {
  type CreateCampaignBody,
  type ProbeProtocol,
  useCreateCampaign,
  usePreviewDispatchCount,
  useStartCampaign,
} from "@/api/hooks/campaigns";
import {
  useCatalogueFacets,
  useCatalogueListInfinite,
  useCatalogueMap,
} from "@/api/hooks/catalogue";
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
import { useToastStore } from "@/stores/toast";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const WALK_PAGE_SIZE = 500;
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

  // --- Map dialog state --------------------------------------------------
  const [mapOpenFor, setMapOpenFor] = useState<MapTarget>(null);
  const [mapViewport, setMapViewport] = useState<{ bbox: Bbox; zoom: number } | null>(null);

  // --- Dispatch flow state ----------------------------------------------
  const [draftCampaignId, setDraftCampaignId] = useState<string | null>(null);
  const [confirmDialogOpen, setConfirmDialogOpen] = useState(false);
  const [autoStartAfterCreate, setAutoStartAfterCreate] = useState(false);

  // --- Destination-walk state -------------------------------------------
  const [walkProgress, setWalkProgress] = useState<{ collected: number; total: number } | null>(
    null,
  );
  // The last walk failure, persisted so the UI can render an inline alert
  // in addition to the one-shot toast. Cleared on every new walk.
  const [walkError, setWalkError] = useState<Error | null>(null);
  // Tracks whether a walk is already running so rapid double-clicks are
  // ignored. Must be a ref (not state) so the guard fires in the same tick
  // as the first click — a state update is queued for the next render.
  const walkRunningRef = useRef(false);

  // --- Hooks --------------------------------------------------------------
  const createMutation = useCreateCampaign();
  const startMutation = useStartCampaign();
  const previewQuery = usePreviewDispatchCount(draftCampaignId ?? undefined);

  // Pre-submit approximate destination total: uses the first page's `total`
  // exactly like `DestinationPanel` so the SizePreview stays consistent
  // with what the panel footer already advertises. Once the operator has
  // pasted IPs or explicitly selected some, that count wins.
  const destQuery = useMemo(() => destinationFilterToQuery(destFilter), [destFilter]);

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
    const q = destinationFilterToQuery(destFilter);
    // The map endpoint does not carry `shapes` or `city` — mirror Catalogue.tsx.
    // Those two keys stay out of the projection below.
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

  // --- Destination-walk ("Add all across all pages") ---------------------
  //
  // The composer owns a secondary `useCatalogueListInfinite` for the
  // exhaustive walk so the DestinationPanel's shipped "Add all (loaded
  // pages only)" semantics stay intact (plan F.7 §608). Driving the walk
  // through the same hook the panel uses means tests mock one place
  // (the hook) instead of reaching into the openapi-fetch client.

  const walkInfinite = useCatalogueListInfinite(destQuery, {
    pageSize: WALK_PAGE_SIZE,
  });

  // Plain function (no `useCallback`) — `walkInfinite` is the full hook
  // result, which TanStack Query reshapes every render, so memoizing on
  // it defeats the point. The handler isn't threaded through a prop, so
  // referential stability doesn't matter here.
  const handleAddAllDestinationsExhaustive = async () => {
    if (walkRunningRef.current) return;
    walkRunningRef.current = true;
    // Clear any prior failure so a retry starts from a clean slate.
    setWalkError(null);

    try {
      // Seed progress from whatever's already loaded by the hook.
      const seedPages = walkInfinite.data?.pages ?? [];
      const seedTotal = seedPages[0]?.total ?? 0;
      let collectedCount = seedPages.reduce((n, p) => n + p.entries.length, 0);
      setWalkProgress({ collected: collectedCount, total: seedTotal });

      // Drain remaining pages via `fetchNextPage`. Each call returns the
      // updated InfiniteData snapshot, so we read the cumulative page
      // list off the result rather than racing the hook's state.
      let hasNext = walkInfinite.hasNextPage;
      let latestPages = seedPages;
      while (hasNext) {
        const result = await walkInfinite.fetchNextPage();
        latestPages = result.data?.pages ?? latestPages;
        collectedCount = latestPages.reduce((n, p) => n + p.entries.length, 0);
        const total = latestPages[0]?.total ?? seedTotal;
        setWalkProgress({ collected: collectedCount, total });
        hasNext = result.hasNextPage ?? false;
      }

      const collectedIps = new Set<string>();
      for (const page of latestPages) {
        for (const entry of page.entries) collectedIps.add(entry.ip);
      }
      setDestSet(collectedIps);
      setWalkProgress(null);
    } catch (err) {
      const error = err instanceof Error ? err : new Error(String(err));
      // Persisted `walkError` drives an inline alert; the toast is the
      // noisier UX notification. Both surface the same failure, never
      // one without the other.
      setWalkError(error);
      useToastStore.getState().pushToast({ kind: "error", message: error.message });
      setWalkProgress(null);
    } finally {
      walkRunningRef.current = false;
    }
  };

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
      loss_threshold_pct: knobs.loss_threshold_pct,
      stddev_weight: knobs.stddev_weight,
      evaluation_mode: knobs.evaluation_mode,
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

  // Pre-commit view: show the filter's first-page total so operators see a
  // realistic size before Add-all. Falls back to 0 only when the catalogue
  // query hasn't returned yet. SizePreview handles the `~` branch itself.
  const approxDestTotal =
    destSet.size > 0 ? destSet.size : (walkInfinite.data?.pages[0]?.total ?? 0);

  return (
    <form onSubmit={handleStart} aria-label="Create campaign" className="flex flex-col gap-4">
      <header className="flex flex-wrap items-baseline justify-between gap-3">
        <h1 className="text-2xl font-semibold">Create campaign</h1>
        <Button
          type="button"
          variant="outline"
          onClick={handleAddAllDestinationsExhaustive}
          disabled={walkProgress !== null}
        >
          Add all destinations (all pages)
        </Button>
      </header>

      {walkProgress !== null ? (
        <p
          role="status"
          aria-live="polite"
          className="rounded-md border border-dashed bg-muted/30 px-3 py-2 text-sm"
        >
          Collecting {walkProgress.collected} of {destFilter.shapes.length > 0 ? "~" : ""}
          {walkProgress.total} destinations…
        </p>
      ) : null}

      {walkError !== null ? (
        <div
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          Walk failed: {walkError.message}
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
            />
          </Card>
        </div>

        {/* Right column — knobs, preview, action bar */}
        <div className="flex flex-col gap-4">
          <Card className="p-4">
            <KnobPanel value={knobs} onChange={setKnobs} />
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
              onClick={() => {
                void navigate({ to: "/campaigns" });
              }}
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
