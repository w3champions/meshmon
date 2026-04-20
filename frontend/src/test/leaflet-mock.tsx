import type L from "leaflet";
import type { ReactNode } from "react";

interface MapContainerProps {
  children?: ReactNode;
  center?: [number, number];
  zoom?: number;
  minZoom?: number;
  worldCopyJump?: boolean;
  scrollWheelZoom?: boolean;
  className?: string;
}

interface MarkerProps {
  children?: ReactNode;
  position: [number, number];
  eventHandlers?: Record<string, (...args: unknown[]) => void>;
  // L.DivIcon / L.Icon — opaque in the mock.
  icon?: unknown;
}

interface TileLayerProps {
  url?: string;
  attribution?: string;
}

interface PopupProps {
  children?: ReactNode;
}

type EventHandler = (...args: unknown[]) => void;

/**
 * Shape of the fake `L.Map` the mock hands back from `useMap()`.
 *
 * `__fire` and `__drawnLayers` are test affordances — real react-leaflet
 * doesn't expose them. They let a test drive the `pm:create | pm:edit |
 * pm:remove` event pipeline and seed `getGeomanDrawLayers()` output so
 * `GeomanController`'s `collectShapes()` has something to find.
 */
export interface MockLeafletMap {
  fitBounds: () => void;
  /**
   * `setView` accepts the real Leaflet shape (`[lat, lng]` + optional
   * zoom) so assertions can inspect `__setViewCalls` and verify the
   * viewport followed a controlled value change.
   */
  setView: (center: [number, number], zoom?: number) => void;
  /**
   * Ordered log of every `setView` call since `resetLeafletMock()` —
   * used by `LocationPicker` tests to assert the viewport recenters
   * when the controlled `value` prop changes.
   */
  __setViewCalls: Array<{ center: [number, number]; zoom?: number }>;
  on: (event: string, handler: EventHandler) => void;
  off: (event: string, handler: EventHandler) => void;
  removeLayer: (layer: L.Layer) => void;
  getContainer: () => HTMLElement;
  getZoom: () => number;
  setZoom: (zoom: number) => void;
  /** Stub that returns the seeded `__bounds` as an `L.LatLngBounds`-like. */
  getBounds: () => {
    getSouthWest: () => { lat: number; lng: number };
    getNorthEast: () => { lat: number; lng: number };
  };
  options: { zoomSnap: number };
  pm: {
    addControls: () => void;
    removeControls: () => void;
    disableDraw: () => void;
    disableGlobalEditMode: () => void;
    disableGlobalRemovalMode: () => void;
    disableGlobalDragMode: () => void;
    getGeomanDrawLayers: (arg?: boolean) => L.Layer[];
  };
  __fire: (event: string) => void;
  __drawnLayers: L.Layer[];
  __handlers: Map<string, Set<EventHandler>>;
  __container: HTMLElement;
  __zoom: number;
  /**
   * Seed the bbox returned by `getBounds()`. Tuple order matches the
   * frontend `Bbox` wire contract: `[minLat, minLng, maxLat, maxLng]`.
   */
  __bounds: [number, number, number, number];
}

function createMockMap(): MockLeafletMap {
  const handlers = new Map<string, Set<EventHandler>>();
  const drawnLayers: L.Layer[] = [];
  const container =
    typeof document !== "undefined" ? document.createElement("div") : ({} as HTMLElement);
  let zoom = 2;
  // Default viewport: world-ish bounds. Tests seed `__bounds` to pin
  // the emitted viewport for assertions on `onViewportChange`.
  let bounds: [number, number, number, number] = [-60, -180, 70, 180];

  const setViewCalls: Array<{ center: [number, number]; zoom?: number }> = [];

  const map: MockLeafletMap = {
    fitBounds: () => {},
    setView: (center, zoom) => {
      setViewCalls.push({ center, zoom });
    },
    __setViewCalls: setViewCalls,
    on: (event, handler) => {
      let set = handlers.get(event);
      if (!set) {
        set = new Set();
        handlers.set(event, set);
      }
      set.add(handler);
    },
    off: (event, handler) => {
      handlers.get(event)?.delete(handler);
    },
    removeLayer: (layer) => {
      const idx = drawnLayers.indexOf(layer);
      if (idx >= 0) drawnLayers.splice(idx, 1);
    },
    getContainer: () => container,
    getZoom: () => zoom,
    setZoom: (next) => {
      zoom = next;
    },
    getBounds: () => ({
      getSouthWest: () => ({ lat: bounds[0], lng: bounds[1] }),
      getNorthEast: () => ({ lat: bounds[2], lng: bounds[3] }),
    }),
    options: { zoomSnap: 1 },
    pm: {
      addControls: () => {},
      removeControls: () => {},
      disableDraw: () => {},
      disableGlobalEditMode: () => {},
      disableGlobalRemovalMode: () => {},
      disableGlobalDragMode: () => {},
      getGeomanDrawLayers: () => drawnLayers.slice(),
    },
    __fire: (event) => {
      const set = handlers.get(event);
      if (!set) return;
      for (const handler of set) handler();
    },
    __drawnLayers: drawnLayers,
    __handlers: handlers,
    __container: container,
    get __zoom() {
      return zoom;
    },
    set __zoom(next: number) {
      zoom = next;
    },
    get __bounds() {
      return bounds;
    },
    set __bounds(next: [number, number, number, number]) {
      bounds = next;
    },
  };

  return map;
}

// One map instance per test render. Reset via `resetLeafletMock()` between
// tests that need a clean slate; tests that only render once can just read
// `getLeafletMock()` to reach the live instance.
let currentMap: MockLeafletMap = createMockMap();

export function getLeafletMock(): MockLeafletMap {
  return currentMap;
}

export function resetLeafletMock(): void {
  currentMap = createMockMap();
  latestMapEventsHandlers = {};
  latestMarkerDragEnd = null;
}

/**
 * Fire a synthetic map click through the handler most recently registered
 * via `useMapEvents({ click })`. Mirrors the real react-leaflet shape:
 * handlers receive a `LeafletMouseEvent` with `latlng.lat` / `latlng.lng`.
 */
export function fireMapClick(lat: number, lng: number): void {
  const handler = latestMapEventsHandlers.click;
  if (!handler) return;
  handler({ latlng: { lat, lng } });
}

/**
 * Fire a synthetic marker `dragend` through the handler supplied via
 * `<Marker eventHandlers={{ dragend }}>`. Real Leaflet dragend hands the
 * handler a `DragEndEvent` whose `target.getLatLng()` yields the new
 * position; the mock matches that shape.
 */
export function fireMarkerDragEnd(lat: number, lng: number): void {
  const handler = latestMarkerDragEnd;
  if (!handler) return;
  handler({ target: { getLatLng: () => ({ lat, lng }) } });
}

let latestMapEventsHandlers: Record<string, (event: unknown) => void> = {};
let latestMarkerDragEnd: ((event: unknown) => void) | null = null;

export const LeafletMock = {
  MapContainer: ({ children, center, zoom, className }: MapContainerProps) => (
    <div
      data-testid="map-container"
      data-center={center?.join(",")}
      data-zoom={zoom}
      className={className}
    >
      {children}
    </div>
  ),
  TileLayer: ({ url, attribution }: TileLayerProps) => (
    <div data-testid="tile-layer" data-url={url} data-attribution={attribution} />
  ),
  useMapEvents: (handlers: Record<string, (event: unknown) => void>) => {
    latestMapEventsHandlers = { ...handlers };
    return currentMap;
  },
  Marker: ({ children, position, eventHandlers, icon }: MarkerProps) => {
    const onClick = eventHandlers?.click;
    // Capture the most recent dragend handler so `fireMarkerDragEnd`
    // can drive it from tests.
    latestMarkerDragEnd = eventHandlers?.dragend ?? null;
    // Non-interactive container when the marker has no click handler;
    // otherwise delegate to the interactive Marker below so a11y rules
    // are not violated by attaching click handlers to a plain div.
    if (!onClick) {
      return (
        <div
          data-testid="marker"
          data-lat={position[0]}
          data-lon={position[1]}
          data-has-icon={icon ? "true" : "false"}
        >
          {children}
        </div>
      );
    }
    return (
      <button
        type="button"
        data-testid="marker"
        data-lat={position[0]}
        data-lon={position[1]}
        data-has-icon={icon ? "true" : "false"}
        // Marker clicks in Leaflet come through `eventHandlers.click`.
        // Surfacing them via a real button so accessible-role queries
        // and `userEvent.click` both work through the mock.
        onClick={() => onClick()}
      >
        {children}
      </button>
    );
  },
  Popup: ({ children }: PopupProps) => <div data-testid="popup">{children}</div>,
  // Stub react-leaflet's useMap so components calling fitBounds (or
  // geoman's map.pm) don't crash under jsdom. The map is never rendered
  // in unit tests. `pm` mirrors just enough of the geoman surface area
  // that DrawMap's GeomanController can mount and tear down cleanly.
  // Tests that need to drive events call `getLeafletMock()` to reach
  // the same instance and invoke `__fire(...)` / mutate `__drawnLayers`.
  useMap: () => currentMap,
};

interface MarkerClusterGroupMockProps {
  children?: ReactNode;
  zoomToBoundsOnClick?: boolean;
  onClick?: (event: unknown) => void;
}

/**
 * Latest cluster-group onClick captured by the mock. Tests use
 * `fireClusterClick(ids)` to synthesise a cluster click and drive the
 * component's `onClusterClick` handler end-to-end.
 */
let latestClusterOnClick: ((event: unknown) => void) | null = null;

export function fireClusterClick(pinIds: string[]): void {
  const handler = latestClusterOnClick;
  if (!handler) return;
  // `DrawMap`'s handler narrows the cluster layer via duck-typing on
  // `getAllChildMarkers`, so the fake cluster just needs that shape.
  const cluster = {
    getAllChildMarkers: () => pinIds.map((id) => ({ options: { meshmonPinId: id } })),
  };
  handler({ propagatedFrom: cluster, layer: cluster });
}

export function MarkerClusterGroupMock({
  children,
  zoomToBoundsOnClick,
  onClick,
}: MarkerClusterGroupMockProps) {
  latestClusterOnClick = onClick ?? null;
  return (
    <div
      data-testid="marker-cluster-group"
      data-zoom-to-bounds-on-click={
        zoomToBoundsOnClick === undefined ? "" : String(zoomToBoundsOnClick)
      }
      data-has-on-click={onClick ? "true" : "false"}
    >
      {children}
    </div>
  );
}
