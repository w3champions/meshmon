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
      // Callers must also sync document.documentElement.classList ('dark');
      // see ThemeToggle / main.tsx.
      setTheme: (theme) => set({ theme }),
      toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
    }),
    { name: "meshmon-ui" },
  ),
);
