import createClient, { type Middleware } from "openapi-fetch";
import type { paths } from "@/api/schema.gen";
import { useAuthStore } from "@/stores/auth";
import { useToastStore } from "@/stores/toast";

const LOGIN_PATH = "/api/auth/login";

const authMiddleware: Middleware = {
  async onResponse({ request, response }) {
    const isLoginPath = new URL(request.url).pathname === LOGIN_PATH;
    // 401 on login is "wrong credentials" — form surfaces the error inline.
    // 429 and 5xx on login still flow through so users see the retry banner
    // or service-error toast.
    if (response.status === 401 && isLoginPath) {
      return;
    }
    if (response.status === 401) {
      useAuthStore.getState().clearSession();
      // Preserve return path when bouncing.
      const returnTo = window.location.pathname + window.location.search;
      // `startsWith` covers `/login`, `/login/`, and any future `/login/*`
      // sub-path so a re-fired 401 on the login page can't loop.
      if (!window.location.pathname.startsWith("/login")) {
        window.location.assign(`/login?returnTo=${encodeURIComponent(returnTo)}`);
      }
      return;
    }
    if (response.status === 429) {
      const retryAfter = response.headers.get("Retry-After");
      const seconds = retryAfter ? Number.parseInt(retryAfter, 10) : 60;
      useToastStore.getState().pushToast({
        kind: "error",
        message: "Too many requests",
        description: `Try again in ${Number.isFinite(seconds) ? seconds : 60}s.`,
      });
      return;
    }
    if (response.status >= 500) {
      useToastStore.getState().pushToast({
        kind: "error",
        message: "Service error",
        description: `HTTP ${response.status}`,
      });
    }
  },
};

// Use the current origin as baseUrl so request URLs are always absolute.
// In the browser this is equivalent to `baseUrl: "/"` (same-origin `/api/…`),
// and in jsdom-based tests this avoids the relative-URL `TypeError` thrown
// by Node's `Request` constructor when the schema path is left relative.
// No trailing slash: schema paths already start with `/`.
//
// The `fetch` indirection forwards each call through the *current*
// `globalThis.fetch`, which lets tests replace it with `vi.spyOn(global, "fetch")`
// at call time. Without this, openapi-fetch would capture the original jsdom
// fetch reference at module load and ignore later spies.
export const api = createClient<paths>({
  baseUrl: window.location.origin,
  credentials: "include",
  headers: { Accept: "application/json" },
  fetch: (...args) => globalThis.fetch(...args),
});

api.use(authMiddleware);
