import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach } from "vitest";
import { useAuthStore } from "@/stores/auth";
import { useUiStore } from "@/stores/ui";

// Radix UI uses pointer APIs + scrollIntoView that jsdom doesn't implement.
// Polyfill them here so components like Select / ToggleGroup work in tests.
if (typeof Element !== "undefined") {
  if (!Element.prototype.hasPointerCapture) {
    Element.prototype.hasPointerCapture = () => false;
  }
  if (!Element.prototype.setPointerCapture) {
    Element.prototype.setPointerCapture = () => {};
  }
  if (!Element.prototype.releasePointerCapture) {
    Element.prototype.releasePointerCapture = () => {};
  }
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => {};
  }
}

beforeEach(() => {
  useAuthStore.getState().clearSession();
  // reset sidebar + theme to defaults
  useUiStore.setState({ theme: "dark", sidebarCollapsed: false });
});

afterEach(() => {
  sessionStorage.clear();
  localStorage.clear();
});
