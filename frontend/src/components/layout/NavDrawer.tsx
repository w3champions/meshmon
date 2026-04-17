import { Link } from "@tanstack/react-router";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { useUiStore } from "@/stores/ui";
import { navItems } from "./nav-items";

/** Persistent sidebar for >= md viewports. Hidden below that; see MobileNavSheet. */
export function NavDrawer() {
  const collapsed = useUiStore((s) => s.sidebarCollapsed);
  const toggle = useUiStore((s) => s.toggleSidebar);
  return (
    <aside
      className={cn(
        "hidden md:flex flex-col border-r border-border bg-background transition-all",
        collapsed ? "w-14" : "w-56",
      )}
      data-app-chrome="true"
    >
      <nav className="flex flex-col p-2 space-y-1 flex-1">
        {navItems.map((item) => (
          <Link
            key={item.to}
            to={item.to}
            className="rounded px-3 py-2 text-sm hover:bg-muted"
            activeProps={{ className: "bg-muted font-medium" }}
          >
            {collapsed ? item.label[0] : item.label}
          </Link>
        ))}
      </nav>
      <div className="p-2">
        <Button variant="ghost" size="icon" onClick={toggle} aria-label="Toggle sidebar">
          {collapsed ? <ChevronRight className="h-4 w-4" /> : <ChevronLeft className="h-4 w-4" />}
        </Button>
      </div>
    </aside>
  );
}
