import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
    },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
  },
  server: {
    port: 5173,
    host: "0.0.0.0",
    proxy: {
      // Explicit 127.0.0.1 (not `localhost`) so Node's IPv6-first resolution
      // doesn't route to an unrelated `::<port>` listener. Override the
      // target via `MESHMON_API_PROXY_TARGET` when the service runs on a
      // non-default port (e.g. `scripts/smoke.sh` uses :18080).
      "/api": process.env.MESHMON_API_PROXY_TARGET ?? "http://127.0.0.1:8080",
    },
  },
});
