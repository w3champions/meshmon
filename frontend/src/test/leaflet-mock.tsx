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
  eventHandlers?: Record<string, unknown>;
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
  setView: () => void;
  on: (event: string, handler: EventHandler) => void;
  off: (event: string, handler: EventHandler) => void;
  removeLayer: (layer: L.Layer) => void;
  getContainer: () => HTMLElement;
  getZoom: () => number;
  setZoom: (zoom: number) => void;
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
}

function createMockMap(): MockLeafletMap {
  const handlers = new Map<string, Set<EventHandler>>();
  const drawnLayers: L.Layer[] = [];
  const container =
    typeof document !== "undefined" ? document.createElement("div") : ({} as HTMLElement);
  let zoom = 2;

  const map: MockLeafletMap = {
    fitBounds: () => {},
    setView: () => {},
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
}

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
  Marker: ({ children, position }: MarkerProps) => (
    <div data-testid="marker" data-lat={position[0]} data-lon={position[1]}>
      {children}
    </div>
  ),
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
