import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

// ---------------------------------------------------------------------------
// Module mocks
//
// AppShell composes AppBar + NavDrawer + TanStack Router's `<Outlet/>`. None
// of those collaborators matter for the wiring contract under test — the only
// things we want to assert are that AppShell mounts CatalogueStreamProvider
// (which in turn calls `useCatalogueStream` exactly once) and that it opens
// a single `IpHostnameProvider` EventSource against `/api/hostnames/stream`.
// Stubbing the collaborators keeps the test free of router context, auth
// store hydration, and Leaflet/dom dependencies.
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

// Minimal EventSource stand-in so IpHostnameProvider can open its stream
// under jsdom without a real network connection.
class MockEventSource {
  static urls: string[] = [];
  constructor(public url: string) {
    MockEventSource.urls.push(url);
  }
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

// ---------------------------------------------------------------------------
// Imports AFTER mocks so vi.fn() stubs are in place
// ---------------------------------------------------------------------------

import { useCatalogueStream } from "@/api/hooks/catalogue-stream";
import { AppShell } from "@/components/layout/AppShell";

beforeEach(() => {
  MockEventSource.urls = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("AppShell", () => {
  test("mounts CatalogueStreamProvider, calling useCatalogueStream exactly once", () => {
    render(<AppShell />);

    // Wiring contract: the provider mounts the catalogue SSE subscription
    // once for the entire authenticated subtree. If a future refactor
    // accidentally drops the provider wrap, this assertion regresses.
    expect(useCatalogueStream).toHaveBeenCalledTimes(1);
  });

  test("mounts IpHostnameProvider, opening a single /api/hostnames/stream EventSource", () => {
    render(<AppShell />);

    // Wiring contract: the shared hostname provider owns the one-and-only
    // EventSource against `/api/hostnames/stream`. Catching a regression
    // here avoids accidentally spawning per-page subscriptions.
    expect(MockEventSource.urls).toEqual(["/api/hostnames/stream"]);
  });
});
