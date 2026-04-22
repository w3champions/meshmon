import { Outlet } from "@tanstack/react-router";
import CatalogueStreamProvider from "@/api/CatalogueStreamProvider";
import { IpHostnameProvider } from "@/components/ip-hostname";
import { AppBar } from "./AppBar";
import { NavDrawer } from "./NavDrawer";

/**
 * Auth-gated application shell.
 *
 * Mounts the session-scoped providers that must only open after the user
 * is authenticated:
 * - `CatalogueStreamProvider` wires the catalogue SSE subscription so
 *   every catalogue-derived query sees live invalidations.
 * - `IpHostnameProvider` owns the shared IP → hostname map and the
 *   single `/api/hostnames/stream` EventSource. All render sites resolve
 *   hostnames through this provider; nothing else opens that stream.
 */
export function AppShell() {
  return (
    <CatalogueStreamProvider>
      <IpHostnameProvider>
        <div className="flex flex-col h-full">
          <AppBar />
          <div className="flex flex-1 overflow-hidden">
            <NavDrawer />
            <main className="flex-1 overflow-auto p-4 md:p-6">
              <Outlet />
            </main>
          </div>
        </div>
      </IpHostnameProvider>
    </CatalogueStreamProvider>
  );
}
