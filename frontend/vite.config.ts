import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
      "@grafana": path.resolve(__dirname, "../grafana"),
    },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
  },
  server: {
    port: 5173,
    host: "0.0.0.0",
    // The `@grafana` alias resolves to `../grafana/`, a sibling of this
    // Vite project root. Vite's default `fs.allow` is `[<root>]`, so the
    // dev server would refuse to serve `../grafana/panels.json`. Allow the
    // meshmon repo root explicitly so `import ... from "@grafana/..."`
    // works under `vite dev` as well as `vite build`.
    fs: {
      allow: [path.resolve(__dirname, "..")],
    },
    proxy: {
      // Explicit 127.0.0.1 (not `localhost`) so Node's IPv6-first resolution
      // doesn't route to an unrelated `::<port>` listener. Override the
      // target via `MESHMON_API_PROXY_TARGET` when the service runs on a
      // non-default port (e.g. `scripts/dev.sh` uses :18080).
      "/api": process.env.MESHMON_API_PROXY_TARGET ?? "http://127.0.0.1:8080",
    },
  },
});
