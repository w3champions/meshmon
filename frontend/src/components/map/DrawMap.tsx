import "@geoman-io/leaflet-geoman-free";
import "@geoman-io/leaflet-geoman-free/dist/leaflet-geoman.css";

import L from "leaflet";
import type { ReactNode } from "react";
import { useEffect, useRef } from "react";
import { MapContainer, Marker, Popup, TileLayer, useMap } from "react-leaflet";
import type { GeoShape } from "@/lib/geo";
import { cn } from "@/lib/utils";

export interface DrawMapPin {
  id: string;
  lat: number;
  lon: number;
  popup?: ReactNode;
}

export interface DrawMapProps {
  shapes: GeoShape[];
  onShapesChange(next: GeoShape[]): void;
  pins?: DrawMapPin[];
  className?: string;
}

const GEOMAN_CONTROL_OPTIONS = {
  position: "topright" as const,
  drawMarker: false,
  drawCircleMarker: false,
  drawPolyline: false,
  drawText: false,
  drawRectangle: true,
  drawPolygon: true,
  drawCircle: true,
  dragMode: true,
  cutPolygon: false,
  rotateMode: false,
  editMode: true,
  removalMode: true,
};

const OSM_TILE_URL = "https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png";
const OSM_ATTRIBUTION = "© OpenStreetMap contributors";

// World-overview default view. Latitude 20 biases slightly north of the
// equator so Europe/North America aren't clipped at typical aspect ratios,
// and longitude 0 keeps the Pacific from splitting across the seam.
const DEFAULT_CENTER: [number, number] = [20, 0];
const DEFAULT_ZOOM = 2;

/**
 * Convert a single Leaflet layer produced by geoman into our typed `GeoShape`.
 *
 * Order matters: `L.Rectangle` extends `L.Polygon`, so it must be checked
 * first. `L.Circle` is independent (extends `CircleMarker`).
 */
function layerToShape(layer: L.Layer): GeoShape | null {
  if (layer instanceof L.Rectangle) {
    const bounds = layer.getBounds();
    const sw = bounds.getSouthWest();
    const ne = bounds.getNorthEast();
    return {
      kind: "rectangle",
      sw: [sw.lng, sw.lat],
      ne: [ne.lng, ne.lat],
    };
  }
  if (layer instanceof L.Circle) {
    const center = layer.getLatLng();
    return {
      kind: "circle",
      center: [center.lng, center.lat],
      radiusMeters: layer.getRadius(),
    };
  }
  if (layer instanceof L.Polygon) {
    // Polygon.getLatLngs() returns LatLng[] | LatLng[][] | LatLng[][][]
    // depending on nesting. Geoman only produces single-ring polygons here,
    // so we unwrap the outermost array safely.
    const raw = layer.getLatLngs();
    const ring = (Array.isArray(raw[0]) ? raw[0] : raw) as L.LatLng[];
    if (ring.length < 3) return null;
    return {
      kind: "polygon",
      coordinates: ring.map((ll) => [ll.lng, ll.lat] as [number, number]),
    };
  }
  return null;
}

/**
 * Pull every currently-drawn geoman shape off the map and project it to
 * `GeoShape[]`. Non-convertible layers are silently skipped.
 */
function collectShapes(map: L.Map): GeoShape[] {
  const layers = map.pm.getGeomanDrawLayers(false) as L.Layer[];
  const out: GeoShape[] = [];
  for (const layer of layers) {
    const shape = layerToShape(layer);
    if (shape) out.push(shape);
  }
  return out;
}

function clearDrawLayers(map: L.Map): void {
  const layers = map.pm.getGeomanDrawLayers(false) as L.Layer[];
  for (const layer of layers) {
    map.removeLayer(layer);
  }
}

interface GeomanControllerProps {
  shapes: GeoShape[];
  onShapesChange(next: GeoShape[]): void;
}

/**
 * Lives inside a `<MapContainer>` so it can grab the `L.Map` instance via
 * `useMap()`. Owns geoman lifecycle: install controls, wire events, and
 * tear down on unmount.
 *
 * Reconciliation contract: the component is uncontrolled for draw — users
 * draw, we emit. The only external signal we react to is `shapes` going
 * from non-empty to empty (the FilterRail "Clear" path), which wipes the
 * draw layers. A `suppressEmit` ref blocks the corresponding `pm:remove`
 * fan-out so we don't loop back through `onShapesChange`.
 */
function GeomanController({ shapes, onShapesChange }: GeomanControllerProps) {
  const map = useMap();
  const suppressEmitRef = useRef(false);
  // Keep a ref to the latest callback so the mount effect doesn't re-run
  // on every parent render.
  const onShapesChangeRef = useRef(onShapesChange);
  useEffect(() => {
    onShapesChangeRef.current = onShapesChange;
  }, [onShapesChange]);

  useEffect(() => {
    map.pm.addControls(GEOMAN_CONTROL_OPTIONS);

    const emit = () => {
      if (suppressEmitRef.current) return;
      onShapesChangeRef.current(collectShapes(map));
    };

    map.on("pm:create", emit);
    map.on("pm:edit", emit);
    map.on("pm:remove", emit);

    return () => {
      map.off("pm:create", emit);
      map.off("pm:edit", emit);
      map.off("pm:remove", emit);
      // Best-effort teardown. `removeControls` is a no-op when not installed.
      try {
        map.pm.removeControls();
        map.pm.disableDraw();
        map.pm.disableGlobalEditMode();
        map.pm.disableGlobalRemovalMode();
        map.pm.disableGlobalDragMode();
      } catch {
        // Geoman may already be detached in jsdom-flavoured unmount paths.
      }
    };
    // Mount once per map instance; callback changes flow through
    // `onShapesChangeRef` so they don't require re-wiring.
  }, [map]);

  // External shapes prop reconciliation.
  //
  // The component is uncontrolled on draw — the user draws and we emit via
  // `onShapesChange`. The only external signal we react to is the
  // FilterRail "Clear" path (N > 0 → 0), which wipes the feature group.
  // A `suppressEmit` ref blocks the resulting `pm:remove` fan-out so the
  // clear doesn't loop back through `onShapesChange`.
  //
  // Gating on a derived boolean (rather than the `shapes` array itself)
  // keeps this effect from re-running on every parent re-render: it only
  // fires when the cleared state toggles.
  const isCleared = shapes.length === 0;
  useEffect(() => {
    if (!isCleared) return;
    const current = map.pm.getGeomanDrawLayers(false) as L.Layer[];
    if (current.length === 0) return;
    suppressEmitRef.current = true;
    try {
      clearDrawLayers(map);
    } finally {
      suppressEmitRef.current = false;
    }
  }, [map, isCleared]);

  return null;
}

export function DrawMap({ shapes, onShapesChange, pins, className }: DrawMapProps) {
  return (
    <div
      className={cn(
        "h-[400px] md:h-[500px] w-full rounded-md border border-border overflow-hidden",
        className,
      )}
      data-testid="draw-map-shell"
    >
      <MapContainer
        center={DEFAULT_CENTER}
        zoom={DEFAULT_ZOOM}
        minZoom={1}
        worldCopyJump
        scrollWheelZoom={false}
        className="h-full w-full"
      >
        <TileLayer url={OSM_TILE_URL} attribution={OSM_ATTRIBUTION} />
        <GeomanController shapes={shapes} onShapesChange={onShapesChange} />
        {pins?.map((pin) => (
          <Marker key={pin.id} position={[pin.lat, pin.lon]}>
            {pin.popup ? <Popup>{pin.popup}</Popup> : null}
          </Marker>
        ))}
      </MapContainer>
    </div>
  );
}
