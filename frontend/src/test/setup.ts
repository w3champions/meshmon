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

// jsdom does not implement `ResizeObserver`. `@tanstack/react-virtual`
// installs a ResizeObserver subscription on the scroll element; without
// one it short-circuits after the initial `getBoundingClientRect()` read
// and never re-measures. We install a no-op ResizeObserver and widen
// `getBoundingClientRect` to a non-zero default so the virtualizer has a
// viewport to calculate against.
if (typeof window !== "undefined" && typeof window.ResizeObserver === "undefined") {
  class ResizeObserverStub {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  // biome-ignore lint/suspicious/noExplicitAny: polyfill for jsdom
  (window as any).ResizeObserver = ResizeObserverStub;
}

// `@tanstack/virtual-core` reads `offsetWidth` / `offsetHeight` on the
// scroll element (see `getRect` in the package). jsdom leaves both at 0,
// which collapses the virtualizer's viewport to nothing and prevents
// rows from ever committing. Override the getters so the scroll element
// reports a plausible viewport under tests.
if (typeof HTMLElement !== "undefined") {
  const originalOffsetWidth = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetWidth");
  const originalOffsetHeight = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype,
    "offsetHeight",
  );
  Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
    configurable: true,
    get() {
      const orig = originalOffsetWidth?.get?.call(this);
      return typeof orig === "number" && orig > 0 ? orig : 1024;
    },
  });
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get() {
      const orig = originalOffsetHeight?.get?.call(this);
      return typeof orig === "number" && orig > 0 ? orig : 800;
    },
  });
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
