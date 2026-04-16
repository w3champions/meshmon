import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "@/api/client";
import { useAuthStore } from "@/stores/auth";

// `vi.hoisted` makes `pushToast` available inside the hoisted `vi.mock`
// factory below, while also exposing it at module scope so individual tests
// can assert against it directly instead of re-walking the mocked store.
const { pushToast } = vi.hoisted(() => ({ pushToast: vi.fn() }));

vi.mock("@/stores/toast", () => ({
  useToastStore: {
    getState: () => ({ pushToast }),
  },
}));

const originalLocation = window.location;

beforeEach(() => {
  // jsdom's location is not writable by default — re-define.
  // `origin` is kept because `api/client.ts` reads it at module-load time to
  // derive the absolute baseUrl.
  Object.defineProperty(window, "location", {
    configurable: true,
    value: {
      pathname: "/agents",
      search: "",
      origin: "http://localhost:3000",
      assign: vi.fn(),
    },
    writable: true,
  });
});

afterEach(() => {
  Object.defineProperty(window, "location", {
    configurable: true,
    value: originalLocation,
    writable: true,
  });
  pushToast.mockClear();
  vi.restoreAllMocks();
});

describe("api middleware", () => {
  it("401 on protected path clears session and redirects", async () => {
    useAuthStore.getState().setSession({ username: "admin" });
    vi.spyOn(global, "fetch").mockResolvedValue(new Response(null, { status: 401 }));
    await api.GET("/api/web-config");
    expect(useAuthStore.getState().isAuthenticated).toBe(false);
    expect(window.location.assign).toHaveBeenCalledWith("/login?returnTo=%2Fagents");
  });

  it("401 on /api/auth/login does not redirect", async () => {
    vi.spyOn(global, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "invalid credentials" }), {
        status: 401,
        headers: { "content-type": "application/json" },
      }),
    );
    await api.POST("/api/auth/login", {
      body: { username: "admin", password: "wrong" },
    });
    expect(window.location.assign).not.toHaveBeenCalled();
  });

  it("429 pushes a retry toast with parsed Retry-After seconds", async () => {
    vi.spyOn(global, "fetch").mockResolvedValue(
      new Response(null, { status: 429, headers: { "retry-after": "42" } }),
    );
    await api.GET("/api/web-config");
    expect(pushToast).toHaveBeenCalledWith({
      kind: "error",
      message: "Too many requests",
      description: "Try again in 42s.",
    });
  });

  it("5xx pushes a service-error toast", async () => {
    vi.spyOn(global, "fetch").mockResolvedValue(new Response(null, { status: 503 }));
    await api.GET("/api/web-config");
    expect(pushToast).toHaveBeenCalledWith({
      kind: "error",
      message: "Service error",
      description: "HTTP 503",
    });
  });
});
