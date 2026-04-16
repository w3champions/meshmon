import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach } from "vitest";
import { useAuthStore } from "@/stores/auth";
import { useUiStore } from "@/stores/ui";

beforeEach(() => {
  useAuthStore.getState().clearSession();
  // reset sidebar + theme to defaults
  useUiStore.setState({ theme: "dark", sidebarCollapsed: false });
});

afterEach(() => {
  sessionStorage.clear();
  localStorage.clear();
});
