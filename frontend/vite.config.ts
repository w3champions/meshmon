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
      // doesn't route to an unrelated `::8080` listener (e.g. Docker Desktop
      // commonly binds `*:8080` on IPv6 while the service binds IPv4 only).
      "/api": "http://127.0.0.1:8080",
    },
  },
});
