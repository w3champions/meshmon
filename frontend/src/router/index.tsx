import type { QueryClient } from "@tanstack/react-query";
import {
  createRootRouteWithContext,
  createRoute,
  createRouter,
  isRedirect,
  Outlet,
  redirect,
} from "@tanstack/react-router";
import { api } from "@/api/client";
import { AppShell } from "@/components/layout/AppShell";
import Login from "@/pages/Login";
import NotFound from "@/pages/NotFound";
import Overview from "@/pages/Overview";

interface RouterContext {
  queryClient: QueryClient;
}

const rootRoute = createRootRouteWithContext<RouterContext>()({
  component: () => <Outlet />,
  notFoundComponent: NotFound,
});

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/login",
  component: Login,
});

const authRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "auth-guard",
  beforeLoad: async ({ location, context }) => {
    // Preserve both pathname and search params so `?filter=active` survives
    // an auth bounce. `searchStr` already includes the leading "?".
    const returnTo = location.pathname + location.searchStr;

    try {
      const data = await context.queryClient.fetchQuery({
        queryKey: ["web-config"],
        queryFn: async () => {
          const { data, error, response } = await api.GET("/api/web-config");
          // Only treat 401 as "needs login". Network failures / 5xx fall
          // through to the router's error boundary or the toast middleware.
          if (response?.status === 401) {
            throw redirect({ to: "/login", search: { returnTo } });
          }
          if (error || !data) {
            throw error ?? new Error("web-config: no data");
          }
          return data;
        },
        staleTime: Number.POSITIVE_INFINITY,
        retry: false,
      });
      return { webConfig: data };
    } catch (err) {
      // Re-throw redirect objects so TanStack Router can handle navigation.
      if (isRedirect(err)) {
        throw err;
      }
      // Network failures / 5xx: bubble up to the router error boundary.
      throw err;
    }
  },
  component: AppShell,
});

const overviewRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/",
  component: Overview,
});

const agentsRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/agents",
  component: () => <p>Coming soon.</p>,
});

const alertsRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/alerts",
  component: () => <p>Coming soon.</p>,
});

const routeTree = rootRoute.addChildren([
  loginRoute,
  authRoute.addChildren([overviewRoute, agentsRoute, alertsRoute]),
]);

export function createAppRouter(queryClient: QueryClient) {
  return createRouter({ routeTree, context: { queryClient } });
}

// Default export for backwards-compatibility during migration (no queryClient context).
// main.tsx should use createAppRouter instead.
export const router = createAppRouter(
  // Lazy import at module level isn't possible; this placeholder will be
  // replaced when main.tsx passes the real QueryClient via createAppRouter.
  // biome-ignore lint/suspicious/noExplicitAny: bootstrap placeholder
  undefined as any,
);

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof createAppRouter>;
  }
}
