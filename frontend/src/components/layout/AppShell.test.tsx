import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

// ---------------------------------------------------------------------------
// Module mocks
//
// AppShell composes AppBar + NavDrawer + TanStack Router's `<Outlet/>`. None
// of those collaborators matter for the wiring contract under test — the only
// thing we want to assert is that AppShell mounts CatalogueStreamProvider,
// which in turn calls `useCatalogueStream` exactly once. Stubbing the
// collaborators keeps the test free of router context, auth store hydration,
// and Leaflet/dom dependencies.
// ---------------------------------------------------------------------------

vi.mock("@/api/hooks/catalogue-stream", () => ({
  useCatalogueStream: vi.fn(),
}));

vi.mock("@tanstack/react-router", () => ({
  Outlet: () => <div data-testid="outlet-stub" />,
}));

vi.mock("@/components/layout/AppBar", () => ({
  AppBar: () => <header data-testid="appbar-stub" />,
}));

vi.mock("@/components/layout/NavDrawer", () => ({
  NavDrawer: () => <nav data-testid="navdrawer-stub" />,
}));

// ---------------------------------------------------------------------------
// Imports AFTER mocks so vi.fn() stubs are in place
// ---------------------------------------------------------------------------

import { useCatalogueStream } from "@/api/hooks/catalogue-stream";
import { AppShell } from "@/components/layout/AppShell";

afterEach(() => {
  vi.clearAllMocks();
});

describe("AppShell", () => {
  test("mounts CatalogueStreamProvider, calling useCatalogueStream exactly once", () => {
    render(<AppShell />);

    // Wiring contract: the provider mounts the catalogue SSE subscription
    // once for the entire authenticated subtree. If a future refactor
    // accidentally drops the provider wrap, this assertion regresses.
    expect(useCatalogueStream).toHaveBeenCalledTimes(1);
  });
});
