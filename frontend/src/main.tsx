import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Toaster } from "@/components/ui/sonner";
import { createAppRouter } from "@/router";
import { useUiStore } from "@/stores/ui";
import "@/styles/globals.css";

// Apply persisted theme before first paint to prevent flash.
const { theme } = useUiStore.getState();
document.documentElement.classList.toggle("dark", theme === "dark");

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 10_000,
      refetchOnWindowFocus: false,
    },
  },
});

// Pass queryClient as router context so beforeLoad guards can dedup
// auth probes with component-level reads via the same ["web-config"] cache key.
const router = createAppRouter(queryClient);

const rootEl = document.getElementById("app");
if (!rootEl) {
  throw new Error("#app element missing from index.html");
}

createRoot(rootEl).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
      <Toaster />
    </QueryClientProvider>
  </StrictMode>,
);
