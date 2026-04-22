import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { render } from "@testing-library/react";
import type { ReactElement, ReactNode } from "react";
import { IpHostnameProvider } from "@/components/ip-hostname";

/**
 * Renders `ui` under a fresh `QueryClient` and the shared
 * `<IpHostnameProvider>`. The providers mount via RTL's `wrapper` option
 * (not as parents of `ui` in the render call) so the returned `rerender`
 * helper preserves them — without this, a rerender with a naked element
 * drops the provider tree and consumers throw "IpHostnameProvider missing".
 */
export function renderWithQuery(ui: ReactElement) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: 0 },
      mutations: { retry: false },
    },
  });
  function Wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={client}>
        <IpHostnameProvider>{children}</IpHostnameProvider>
      </QueryClientProvider>
    );
  }
  return render(ui, { wrapper: Wrapper });
}

export function renderWithProviders(ui: ReactElement, initialPath = "/") {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: 0 },
      mutations: { retry: false },
    },
  });

  const rootRoute = createRootRoute({ component: Outlet });
  const testRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => ui,
  });
  // Placeholder routes so <Link to="/alerts"> and <Link to="/paths/$source/$target"> resolve.
  const alertsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/alerts",
    component: () => null,
  });
  const pathsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/paths/$source/$target",
    component: () => null,
  });
  const compareRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/paths/$source/$target/routes/compare",
    component: () => null,
  });
  const reportPathRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/reports/path",
    component: () => null,
  });
  const agentRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/agents/$id",
    component: () => null,
  });
  const agentsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/agents",
    component: () => null,
  });

  const router = createRouter({
    routeTree: rootRoute.addChildren([
      testRoute,
      alertsRoute,
      pathsRoute,
      compareRoute,
      reportPathRoute,
      agentRoute,
      agentsRoute,
    ]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
  });

  return render(
    <QueryClientProvider client={client}>
      <IpHostnameProvider>
        <RouterProvider router={router} />
      </IpHostnameProvider>
    </QueryClientProvider>,
  );
}
