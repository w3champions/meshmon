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
import CampaignComposer from "@/pages/CampaignComposer";
import CampaignDetail from "@/pages/CampaignDetail";
import Campaigns from "@/pages/Campaigns";
import Catalogue from "@/pages/Catalogue";
import HistoryPair from "@/pages/HistoryPair";
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

// The composer page owns all draft state in memory — the URL does not
// persist selections, knobs, or the pending create campaign id across
// navigation. No `validateSearch` is needed.
export const campaignNewRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/campaigns/new",
  component: CampaignComposer,
});

// Campaign detail page. The page renders a four-tab shell
// (Candidates/Pairs/Raw/Settings) below the header card and action bar.
// `tab` drives the active panel; the Raw-tab filter params live on the same
// schema so a tab switch preserves them instead of racing with a separate
// declaration.
//
// Per-field `.catch` is used on every enum so an invalid value on ONE field
// falls back to that field's default without dropping sibling fields. A
// whole-object `.catch({})` would reset every field when any field is
// invalid — e.g. `?tab=bogus&raw_state=pending` would lose `raw_state`
// along with the bad `tab`. Per-field resilience keeps valid neighbours
// intact. Every field is also `.optional()` at the TYPE level so callers
// that navigate to `/campaigns/$id` without a search clause still type-
// check (TanStack Router gates `search` required/optional on whether the
// inferred type has any required keys).
export const campaignDetailSearchSchema = z.object({
  tab: z.enum(["candidates", "pairs", "raw", "settings"]).catch("candidates").optional(),
  raw_state: z
    .enum(["pending", "dispatched", "reused", "succeeded", "unreachable", "skipped"])
    .catch(() => undefined as never)
    .optional(),
  raw_protocol: z
    .enum(["icmp", "tcp", "udp"])
    .catch(() => undefined as never)
    .optional(),
  raw_kind: z
    .enum(["campaign", "detail_ping", "detail_mtr"])
    .catch(() => undefined as never)
    .optional(),
  // Candidates-tab sort column + direction. Namespaced (`cand_`) so a
  // future Pairs-tab sort can use its own prefix without stepping on
  // this value. Application default when both fields are absent:
  // `composite_score` / `desc` (resolved in `CandidatesTab`'s
  // `DEFAULT_SORT`). Per-field `.catch(() => undefined)` means a stale
  // shared URL with an unknown enum value (e.g. `?cand_sort=rank` from
  // an earlier build where `rank` was a separate key) falls back to the
  // application default instead of throwing.
  cand_sort: z
    .enum([
      "display_name",
      "destination_ip",
      "city",
      "asn",
      "pairs_improved",
      "avg_improvement_ms",
      "avg_loss_pct",
      "composite_score",
    ])
    .catch(() => undefined as never)
    .optional(),
  cand_dir: z
    .enum(["asc", "desc"])
    .catch(() => undefined as never)
    .optional(),
});

/**
 * Campaign-detail search — every field is `.optional()` so navigations
 * that omit `?tab=…` still type-check. `parseCampaignDetailSearch` fills
 * `tab` to `"candidates"` when absent, so the page always sees a concrete
 * tab value without needing a page-side `??` fallback.
 */
export type CampaignDetailSearch = z.infer<typeof campaignDetailSearchSchema>;

/** Enumeration of the active tab values; the parser fills `undefined` with `"candidates"`. */
export type CampaignDetailTab = "candidates" | "pairs" | "raw" | "settings";

/**
 * Safe default when the WHOLE search object is malformed (e.g. the router
 * hands us something that isn't a plain object). Per-field resilience is
 * handled inside `campaignDetailSearchSchema` via `.catch(...)` — this
 * fallback only fires when the root-level parse throws.
 */
const CAMPAIGN_DETAIL_SEARCH_DEFAULT: CampaignDetailSearch = { tab: "candidates" };

/**
 * Parse the raw URL-search bag. Per-field `.catch` inside the schema drops
 * invalid enum values silently; the outer `.safeParse` guard only fires if
 * the whole object is malformed. We then fill `tab` with `"candidates"`
 * when absent (the schema keeps it `.optional()` at the type level so nav
 * callers don't have to supply `search: { tab: … }` on every `Link`).
 *
 * TanStack Router v1 merges the validator's output onto the raw search (it
 * does NOT replace the source), so we explicitly set every known key on
 * the return — otherwise a URL-supplied `tab=bogus` would survive zod's
 * rejection and resurface downstream. Explicit `undefined` deletes the
 * key cleanly.
 */
function parseCampaignDetailSearch(search: unknown): CampaignDetailSearch {
  const result = campaignDetailSearchSchema.safeParse(search);
  const parsed = result.success ? result.data : CAMPAIGN_DETAIL_SEARCH_DEFAULT;
  return {
    tab: parsed.tab ?? "candidates",
    raw_state: parsed.raw_state,
    raw_protocol: parsed.raw_protocol,
    raw_kind: parsed.raw_kind,
    cand_sort: parsed.cand_sort,
    cand_dir: parsed.cand_dir,
  };
}

export const campaignDetailRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/campaigns/$id",
  component: CampaignDetail,
  validateSearch: (search: Record<string, unknown>): CampaignDetailSearch =>
    parseCampaignDetailSearch(search),
});

// ---------------------------------------------------------------------------
// /history/pair — latency/loss + MTR history for one (source, destination).
// URL is the source of truth for the picker state; Batch 5's Raw-tab drilldown
// links here with `?source=…&destination=…` and expects the page to preseed
// both pickers and kick off the measurements fetch on first render.
// ---------------------------------------------------------------------------
export const historyPairSearchSchema = z
  .object({
    source: z.string().optional(),
    destination: z.string().optional(),
    protocol: z.array(z.enum(["icmp", "tcp", "udp"])).optional(),
    range: z.enum(["24h", "7d", "30d", "90d", "custom"]).catch("30d").default("30d"),
    from: z.string().datetime().optional(),
    to: z.string().datetime().optional(),
  })
  .refine((s) => s.range !== "custom" || (s.from && s.to), {
    message: "custom range requires from and to",
  });

export type HistoryPairSearch = z.infer<typeof historyPairSearchSchema>;

export const historyPairRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/history/pair",
  component: HistoryPair,
  validateSearch: (search) => historyPairSearchSchema.parse(search),
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
    campaignNewRoute,
    campaignDetailRoute,
    historyPairRoute,
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
