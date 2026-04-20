import "leaflet";

declare module "leaflet" {
  interface MarkerOptions {
    /**
     * Catalogue entry ID attached to a Leaflet marker so cluster-click
     * handlers can map child markers back to `CatalogueEntry` rows without
     * any `any` casts.
     */
    meshmonPinId?: string;
  }
}
