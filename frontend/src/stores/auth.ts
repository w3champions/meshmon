import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

interface AuthState {
  isAuthenticated: boolean;
  username: string | null;
  setSession: (opts: { username: string }) => void;
  clearSession: () => void;
}

export const useAuthStore = create<AuthState>()(
  persist(
    (set) => ({
      isAuthenticated: false,
      username: null,
      setSession: ({ username }) => set({ isAuthenticated: true, username }),
      clearSession: () => set({ isAuthenticated: false, username: null }),
    }),
    {
      name: "meshmon-auth",
      storage: createJSONStorage(() => sessionStorage),
    },
  ),
);
