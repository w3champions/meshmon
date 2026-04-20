import L from "leaflet";
import { useCallback, useMemo } from "react";
import type {
  CatalogueEntry,
  CatalogueMapBucket,
  CatalogueMapResponse,
} from "@/api/hooks/catalogue";
import { DrawMap, type DrawMapPin } from "@/components/map/DrawMap";
import type { Bbox, GeoShape } from "@/lib/geo";
import { EntryCard } from "./EntryCard";

export interface CatalogueMapProps {
  /** Latest map response from `useCatalogueMap`; `undefined` while loading. */
  response: CatalogueMapResponse | undefined;
  isLoading: boolean;
  isError: boolean;
  shapes: GeoShape[];
  onShapesChange(next: GeoShape[]): void;
  /**
   * Fires when the operator clicks a detail pin's popup "Open details"
   * button. The full entry is passed (not just the id) so the parent
   * can seed the drawer — `mapQuery` drops `city` and `shapes`, so a
   * visible map pin may represent a row the main table hasn't loaded.
   * Without the seed, `rows.find(id)` misses and the drawer silently
   * fails to open.
   */
  onOpenEntry(entry: CatalogueEntry): void;
  /** Operator clicked a server-side cluster bubble — parent opens dialog. */
  onClusterOpen(cell: Bbox): void;
  /** Parent reads this to drive `useCatalogueMap(bbox, zoom, filters)`. */
  onViewportChange(bbox: Bbox, zoom: number): void;
  className?: string;
}

interface EntryPopupProps {
  entry: CatalogueEntry;
  onOpen(): void;
}

export function EntryPopup({ entry, onOpen }: EntryPopupProps) {
  return (
    <div className="flex w-64 flex-col gap-2 text-sm">
      <EntryCard entry={entry} />
      <button
        type="button"
        className="self-start text-xs underline underline-offset-2 hover:text-foreground"
        aria-label={`Open details for ${entry.ip}`}
        onClick={onOpen}
      >
        Open details
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Cluster markers
// ---------------------------------------------------------------------------

/**
 * Sqrt-scaled radius for a server-side cluster bubble — matches the
 * visual cadence `leaflet.markercluster` uses on small/medium/large
 * groups without committing to Leaflet's CSS classes (the bubble is a
 * standalone `L.divIcon`).
 */
function clusterRadius(count: number): number {
  if (count <= 0) return 18;
  const base = 16;
  return Math.min(42, Math.round(base + Math.sqrt(count) * 2.5));
}

/**
 * Builds a count-bubble `L.DivIcon` for a server-aggregated cluster cell.
 * The icon renders a circular badge of `radius` px carrying the cell's
 * `count` as its label; `data-testid="cluster-bubble"` makes the marker
 * addressable from unit tests.
 */
function buildClusterIcon(bucket: CatalogueMapBucket): L.DivIcon {
  const r = clusterRadius(bucket.count);
  const diameter = r * 2;
  // `role="button"` + `aria-label` expose the bubble to assistive tech —
  // clicks fire via `DrawMapPin.onClick` which is wired directly on the
  // marker (not on this icon HTML), so the click path stays the same
  // while keyboard/screen-reader users get a meaningful description.
  return L.divIcon({
    className: "meshmon-cluster-bubble",
    iconSize: [diameter, diameter],
    iconAnchor: [r, r],
    html: `<div
      role="button"
      aria-label="Open ${bucket.count} entries in this area"
      data-testid="cluster-bubble"
      data-count="${bucket.count}"
      style="width:${diameter}px;height:${diameter}px;line-height:${diameter}px;border-radius:9999px;background:rgba(37,99,235,0.85);color:white;font-weight:600;font-size:12px;text-align:center;border:2px solid rgba(255,255,255,0.9);box-shadow:0 1px 4px rgba(0,0,0,0.2);"
    >${bucket.count}</div>`,
  });
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function CatalogueMap({
  response,
  isLoading,
  isError,
  shapes,
  onShapesChange,
  onOpenEntry,
  onClusterOpen,
  onViewportChange,
  className,
}: CatalogueMapProps) {
  const detailRows = response?.kind === "detail" ? response.rows : undefined;
  const clusters = response?.kind === "clusters" ? response.buckets : undefined;
  const isClusterMode = response?.kind === "clusters";

  // Detail pins — one per catalogue entry with lat/lon. Filtered so
  // incomplete rows don't anchor to `(0, 0)`.
  const detailPins: DrawMapPin[] = useMemo(() => {
    if (!detailRows) return [];
    return detailRows
      .filter((e) => e.latitude != null && e.longitude != null)
      .map((e) => ({
        id: e.id,
        lat: e.latitude as number,
        lon: e.longitude as number,
        popup: <EntryPopup entry={e} onOpen={() => onOpenEntry(e)} />,
      }));
  }, [detailRows, onOpenEntry]);

  // Cluster pins — one per server-aggregated cell. Clicks fire
  // `onClusterOpen(cell)` which the parent threads into the dialog.
  const clusterPins: DrawMapPin[] = useMemo(() => {
    if (!clusters) return [];
    return clusters.map((bucket) => ({
      id: `cluster-${bucket.sample_id}`,
      lat: bucket.lat,
      lon: bucket.lng,
      icon: buildClusterIcon(bucket),
      // `MapBucket.bbox` already uses the `[minLat,minLng,maxLat,maxLng]`
      // order our `Bbox` type expects, but the OpenAPI codegen
      // surfaces it as a plain `number[]`. The backend guarantees the
      // wire shape; assert it at the cast boundary here.
      onClick: () => onClusterOpen(bucket.bbox as Bbox),
    }));
  }, [clusters, onClusterOpen]);

  const pins = isClusterMode ? clusterPins : detailPins;

  /**
   * Detail-mode cluster handler. `react-leaflet-cluster` groups nearby
   * detail pins visually; clicking a cluster should open the same dialog
   * the server-aggregated cluster bubbles use, scoped to the bounding
   * box of the clustered pins. Without this, `MarkerClusterGroup` falls
   * back to its default zoom-to-bounds behavior and the dialog never
   * opens.
   */
  const handleDetailClusterClick = useCallback(
    (pinIds: string[]) => {
      if (!detailRows || pinIds.length === 0) return;
      // Look up each pin's lat/lon (detail pins were filtered to have both).
      let minLat = Number.POSITIVE_INFINITY;
      let maxLat = Number.NEGATIVE_INFINITY;
      let minLng = Number.POSITIVE_INFINITY;
      let maxLng = Number.NEGATIVE_INFINITY;
      for (const id of pinIds) {
        const entry = detailRows.find((e) => e.id === id);
        if (!entry || entry.latitude == null || entry.longitude == null) continue;
        const lat = entry.latitude as number;
        const lng = entry.longitude as number;
        if (lat < minLat) minLat = lat;
        if (lat > maxLat) maxLat = lat;
        if (lng < minLng) minLng = lng;
        if (lng > maxLng) maxLng = lng;
      }
      if (!Number.isFinite(minLat)) return;
      // Pad the bbox by a small epsilon so edge pins stay inside — Leaflet
      // clusters compose out of pins that round to the same visual area,
      // but the backend keyset filter uses a strict BETWEEN, so an exact
      // zero-area bbox would return a single row.
      const epsilon = Math.max(1e-6, (maxLat - minLat) * 0.02, (maxLng - minLng) * 0.02);
      const bbox: Bbox = [minLat - epsilon, minLng - epsilon, maxLat + epsilon, maxLng + epsilon];
      onClusterOpen(bbox);
    },
    [detailRows, onClusterOpen],
  );

  if (isError) {
    return (
      <div
        role="alert"
        className="flex h-[400px] w-full items-center justify-center rounded-md border border-destructive/40 bg-destructive/5 p-4 text-sm text-destructive md:h-[500px]"
      >
        Failed to load map data. Pan or zoom to retry.
      </div>
    );
  }

  // While loading we still render `DrawMap` so operators see their drawn
  // shapes and can pan — only the pins are gated on a populated response.
  return (
    <div className="relative" aria-busy={isLoading}>
      <DrawMap
        shapes={shapes}
        onShapesChange={onShapesChange}
        pins={pins}
        onViewportChange={onViewportChange}
        clusterMode={isClusterMode}
        onClusterClick={isClusterMode ? undefined : handleDetailClusterClick}
        className={className}
      />
    </div>
  );
}
