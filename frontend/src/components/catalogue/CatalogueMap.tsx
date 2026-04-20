import L from "leaflet";
import { useMemo } from "react";
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
  /** Operator clicked a detail pin's popup "Open details" button. */
  onRowClick(id: string): void;
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
  return L.divIcon({
    className: "meshmon-cluster-bubble",
    iconSize: [diameter, diameter],
    iconAnchor: [r, r],
    html: `<div
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
  onRowClick,
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
        popup: <EntryPopup entry={e} onOpen={() => onRowClick(e.id)} />,
      }));
  }, [detailRows, onRowClick]);

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
        className={className}
      />
    </div>
  );
}
