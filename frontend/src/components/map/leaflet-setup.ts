import L from "leaflet";
import iconUrl from "leaflet/dist/images/marker-icon.png";
import iconRetinaUrl from "leaflet/dist/images/marker-icon-2x.png";
import shadowUrl from "leaflet/dist/images/marker-shadow.png";

// leaflet-geoman is a UMD bundle that reads the global `L`. Vite's ESM loader
// does not expose it automatically, so this module attaches it. Import it
// before the geoman side-effect import.
(globalThis as { L?: typeof L }).L = L;

// Leaflet ships its default marker icon via CSS + `require('./images/*')`
// relative paths that resolve at bundle time with Webpack but not with
// Vite's ESM loader. The stock `L.Icon.Default` therefore renders a broken
// image. Re-bind the three asset URLs through Vite's static-asset pipeline
// so markers render correctly.
L.Icon.Default.mergeOptions({
  iconRetinaUrl,
  iconUrl,
  shadowUrl,
});
