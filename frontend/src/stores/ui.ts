import { create } from "zustand";
import { persist } from "zustand/middleware";

interface UiState {
  theme: "dark" | "light";
  sidebarCollapsed: boolean;
  setTheme: (theme: "dark" | "light") => void;
  toggleSidebar: () => void;
}

export const useUiStore = create<UiState>()(
  persist(
    (set) => ({
      theme: "dark",
      sidebarCollapsed: false,
      // Callers must also sync document.documentElement.classList ('light');
      // @theme defaults to dark, so `.light` is the override class.
      // See ThemeToggle / main.tsx.
      setTheme: (theme) => set({ theme }),
      toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
    }),
    { name: "meshmon-ui" },
  ),
);
