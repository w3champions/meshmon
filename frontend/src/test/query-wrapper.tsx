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
import type { ReactElement } from "react";

export function renderWithQuery(ui: ReactElement) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: 0 },
      mutations: { retry: false },
    },
  });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
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
    routeTree: rootRoute.addChildren([testRoute, alertsRoute, pathsRoute, agentRoute, agentsRoute]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
  });

  return render(
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}
