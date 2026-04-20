import L from "leaflet";

// leaflet-geoman is a UMD bundle that reads the global `L`. Vite's ESM loader
// does not expose it automatically, so this module attaches it. Import it
// before the geoman side-effect import.
(globalThis as { L?: typeof L }).L = L;
