import type { QueryClient } from "@tanstack/react-query";
import {
  createRootRouteWithContext,
  createRoute,
  createRouter,
  isRedirect,
  Outlet,
  redirect,
} from "@tanstack/react-router";
import { z } from "zod";
import { api } from "@/api/client";
import { AppShell } from "@/components/layout/AppShell";
import AgentDetail from "@/pages/AgentDetail";
import AgentsList from "@/pages/AgentsList";
import Alerts from "@/pages/Alerts";
import Campaigns from "@/pages/Campaigns";
import Catalogue from "@/pages/Catalogue";
import Login from "@/pages/Login";
import NotFound from "@/pages/NotFound";
import Overview from "@/pages/Overview";
import PathDetail from "@/pages/PathDetail";
import Report from "@/pages/Report";
import RouteCompare from "@/pages/RouteCompare";
import { useAuthStore } from "@/stores/auth";

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
        queryKey: ["session"],
        queryFn: async () => {
          const { data, error, response } = await api.GET("/api/session");
          // Only treat 401 as "needs login". Network failures / 5xx fall
          // through to the router's error boundary or the toast middleware.
          if (response?.status === 401) {
            throw redirect({ to: "/login", search: { returnTo } });
          }
          if (error || !data) {
            throw error ?? new Error("session: no data");
          }
          return data;
        },
        staleTime: Number.POSITIVE_INFINITY,
        retry: false,
      });
      // Hydrate the auth store from the probe so hard-refreshed tabs still
      // know who's signed in (sessionStorage is wiped on hard refresh but
      // the cookie isn't).
      useAuthStore.getState().setSession({ username: data.username });
      return { session: data };
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
  component: AgentsList,
});

export const agentDetailRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/agents/$id",
  component: AgentDetail,
});

const pathDetailSearchSchema = z
  .object({
    range: z.enum(["1h", "6h", "24h", "7d", "30d", "2y", "custom"]).default("24h"),
    from: z.string().datetime().optional(),
    to: z.string().datetime().optional(),
    protocol: z.enum(["icmp", "udp", "tcp"]).optional(),
  })
  .refine((s) => s.range !== "custom" || (s.from && s.to), {
    message: "custom range requires from and to",
  });

export const pathDetailRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/paths/$source/$target",
  component: PathDetail,
  validateSearch: (search) => pathDetailSearchSchema.parse(search),
});

const routeCompareSearchSchema = z.object({
  a: z.coerce.number().int().positive(),
  b: z.coerce.number().int().positive(),
});

export const routeCompareRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/paths/$source/$target/routes/compare",
  component: RouteCompare,
  validateSearch: (search) => routeCompareSearchSchema.parse(search),
});

const reportSearchSchema = z.object({
  source_id: z.string().min(1),
  target_id: z.string().min(1),
  from: z.string().datetime(),
  to: z.string().datetime(),
  protocol: z.enum(["icmp", "udp", "tcp"]).optional(),
});

export const reportRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/reports/path",
  component: Report,
  validateSearch: (search) => reportSearchSchema.parse(search),
});

const alertsRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/alerts",
  component: Alerts,
});

export const catalogueSearchSchema = z.object({
  country: z.array(z.string()).optional(),
  asn: z.array(z.coerce.number()).optional(),
  network: z.array(z.string()).optional(),
  city: z.array(z.string()).optional(),
  ipPrefix: z.string().optional(),
  name: z.string().optional(),
  view: z.enum(["table", "map"]).default("table"),
});

export const catalogueRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/catalogue",
  component: Catalogue,
  validateSearch: (search) => catalogueSearchSchema.parse(search),
});

export const campaignsSearchSchema = z.object({
  q: z.string().optional(),
  state: z.enum(["draft", "running", "completed", "evaluated", "stopped"]).optional(),
  created_by: z.string().optional(),
  sort: z.enum(["title", "created_at", "started_at", "state"]).optional(),
  dir: z.enum(["asc", "desc"]).optional(),
});

export const campaignsRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/campaigns",
  component: Campaigns,
  validateSearch: (search) => campaignsSearchSchema.parse(search),
});

const routeTree = rootRoute.addChildren([
  loginRoute,
  authRoute.addChildren([
    overviewRoute,
    agentsRoute,
    agentDetailRoute,
    pathDetailRoute,
    routeCompareRoute,
    reportRoute,
    alertsRoute,
    catalogueRoute,
    campaignsRoute,
  ]),
]);

export function createAppRouter(queryClient: QueryClient) {
  return createRouter({ routeTree, context: { queryClient } });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof createAppRouter>;
  }
}
