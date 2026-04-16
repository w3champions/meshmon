import { Outlet } from "@tanstack/react-router";
import { AppBar } from "./AppBar";
import { NavDrawer } from "./NavDrawer";

export function AppShell() {
  return (
    <div className="flex flex-col h-full">
      <AppBar />
      <div className="flex flex-1 overflow-hidden">
        <NavDrawer />
        <main className="flex-1 overflow-auto p-4 md:p-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
