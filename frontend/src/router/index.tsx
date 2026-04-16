import {
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  redirect,
} from "@tanstack/react-router";
import { api } from "@/api/client";
import { AppShell } from "@/components/layout/AppShell";
import Login from "@/pages/Login";
import NotFound from "@/pages/NotFound";
import Overview from "@/pages/Overview";

const rootRoute = createRootRoute({
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
  beforeLoad: async ({ location }) => {
    // `response` is undefined on network failure (DNS, timeout, backend down).
    // Short-circuit before dereferencing `response.status`.
    const { data, error, response } = await api.GET("/api/web-config");
    if (!response || response.status === 401 || error) {
      throw redirect({ to: "/login", search: { returnTo: location.pathname } });
    }
    return { webConfig: data };
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

export const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
