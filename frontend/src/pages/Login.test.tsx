import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Login from "@/pages/Login";
import { renderWithQuery } from "@/test/query-wrapper";

const successResponse = () =>
  new Response(JSON.stringify({ username: "alice" }), {
    status: 200,
    headers: { "content-type": "application/json" },
  });

describe("Login page", () => {
  it("rejects empty fields via Zod", async () => {
    const user = userEvent.setup();
    renderWithQuery(<Login />);
    await user.click(screen.getByRole("button", { name: /sign in/i }));
    expect(await screen.findByText(/username is required/i)).toBeInTheDocument();
    expect(await screen.findByText(/password is required/i)).toBeInTheDocument();
  });

  it("shows invalid-credentials message on 401", async () => {
    vi.spyOn(global, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ error: "invalid credentials" }), {
        status: 401,
        headers: { "content-type": "application/json" },
      }),
    );
    const user = userEvent.setup();
    renderWithQuery(<Login />);
    await user.type(screen.getByLabelText(/username/i), "admin");
    await user.type(screen.getByLabelText(/password/i), "nope");
    await user.click(screen.getByRole("button", { name: /sign in/i }));
    expect(await screen.findByText(/invalid credentials/i)).toBeInTheDocument();
  });

  it("shows retry-countdown message on 429", async () => {
    vi.spyOn(global, "fetch").mockResolvedValue(
      new Response(null, { status: 429, headers: { "Retry-After": "30" } }),
    );
    const user = userEvent.setup();
    renderWithQuery(<Login />);
    await user.type(screen.getByLabelText(/username/i), "admin");
    await user.type(screen.getByLabelText(/password/i), "wrong");
    await user.click(screen.getByRole("button", { name: /sign in/i }));
    expect(await screen.findByText(/try again in 30s/i)).toBeInTheDocument();
  });

  describe("open-redirect guard", () => {
    let assignSpy: ReturnType<typeof vi.fn>;
    let originalLocation: Location;

    beforeEach(() => {
      assignSpy = vi.fn();
      originalLocation = window.location;
      vi.spyOn(global, "fetch").mockResolvedValue(successResponse());
    });

    afterEach(() => {
      Object.defineProperty(window, "location", {
        value: originalLocation,
        writable: true,
        configurable: true,
      });
      vi.restoreAllMocks();
    });

    async function loginWith(search: string) {
      // jsdom makes window.location.assign non-configurable; replace the whole
      // location object with a plain mock so we can spy on assign.
      Object.defineProperty(window, "location", {
        value: {
          ...originalLocation,
          search,
          assign: assignSpy,
        },
        writable: true,
        configurable: true,
      });
      const user = userEvent.setup();
      renderWithQuery(<Login />);
      await user.type(screen.getByLabelText(/username/i), "alice");
      await user.type(screen.getByLabelText(/password/i), "secret");
      await user.click(screen.getByRole("button", { name: /sign in/i }));
    }

    it("rejects protocol-relative returnTo (//evil.com) → redirects to /", async () => {
      await loginWith("?returnTo=//evil.com");
      await vi.waitFor(() => expect(assignSpy).toHaveBeenCalledWith("/"));
    });

    it("rejects absolute URL returnTo (https://evil.com) → redirects to /", async () => {
      await loginWith("?returnTo=https%3A%2F%2Fevil.com");
      await vi.waitFor(() => expect(assignSpy).toHaveBeenCalledWith("/"));
    });

    it("allows safe same-origin returnTo (/agents?filter=active)", async () => {
      await loginWith("?returnTo=%2Fagents%3Ffilter%3Dactive");
      await vi.waitFor(() => expect(assignSpy).toHaveBeenCalledWith("/agents?filter=active"));
    });
  });
});
