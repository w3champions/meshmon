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
  useMap: () => ({
    fitBounds: () => {},
    setView: () => {},
    on: () => {},
    off: () => {},
    removeLayer: () => {},
    pm: {
      addControls: () => {},
      removeControls: () => {},
      disableDraw: () => {},
      disableGlobalEditMode: () => {},
      disableGlobalRemovalMode: () => {},
      disableGlobalDragMode: () => {},
      getGeomanDrawLayers: () => [],
    },
  }),
};
