import type { ReactNode } from "react";

interface MapContainerProps {
  children?: ReactNode;
  center?: [number, number];
  zoom?: number;
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
};
