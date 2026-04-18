import "@testing-library/jest-dom/vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderWithProviders } from "@/test/query-wrapper";
import Alerts from "./Alerts";

interface MockResponse {
  url: RegExp;
  status: number;
  body: unknown;
}

function mockFetchSequence(responses: MockResponse[]) {
  return vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = typeof input === "string" ? input : (input as Request).url;
    const hit = responses.find((r) => r.url.test(url));
    if (!hit) throw new Error(`unmocked fetch: ${url}`);
    return new Response(JSON.stringify(hit.body), {
      status: hit.status,
      headers: { "content-type": "application/json" },
    });
  });
}

afterEach(() => vi.restoreAllMocks());

describe("Alerts page", () => {
  it("renders rows and filters by severity", async () => {
    mockFetchSequence([
      {
        url: /\/api\/alerts/,
        status: 200,
        body: [
          {
            fingerprint: "a",
            starts_at: new Date(Date.now() - 60_000).toISOString(),
            ends_at: "0001-01-01T00:00:00Z",
            state: "active",
            labels: {
              alertname: "PathPacketLoss",
              severity: "critical",
              category: "loss",
            },
            summary: "loss on a",
            description: null,
          },
          {
            fingerprint: "b",
            starts_at: new Date(Date.now() - 120_000).toISOString(),
            ends_at: "0001-01-01T00:00:00Z",
            state: "active",
            labels: {
              alertname: "PathLatencyRegression",
              severity: "warning",
              category: "latency",
            },
            summary: "latency on b",
            description: null,
          },
        ],
      },
    ]);

    renderWithProviders(<Alerts />);

    await screen.findByText(/PathPacketLoss/);
    expect(screen.getByText(/PathLatencyRegression/)).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox", { name: /severity/i }));
    await user.click(await screen.findByRole("option", { name: /critical/i }));

    await waitFor(() => {
      expect(screen.queryByText(/PathLatencyRegression/)).toBeNull();
    });
    expect(screen.getByText(/PathPacketLoss/)).toBeInTheDocument();
  });

  it("filters by free-text search across alertname/summary", async () => {
    mockFetchSequence([
      {
        url: /\/api\/alerts/,
        status: 200,
        body: [
          {
            fingerprint: "a",
            starts_at: new Date().toISOString(),
            ends_at: "0001-01-01T00:00:00Z",
            state: "active",
            labels: { alertname: "PathPacketLoss", severity: "critical" },
            summary: "Loss on brazil path",
            description: null,
          },
          {
            fingerprint: "b",
            starts_at: new Date().toISOString(),
            ends_at: "0001-01-01T00:00:00Z",
            state: "active",
            labels: { alertname: "AgentOffline", severity: "warning" },
            summary: "Agent went away",
            description: null,
          },
        ],
      },
    ]);

    renderWithProviders(<Alerts />);
    await screen.findByText(/PathPacketLoss/);

    fireEvent.change(screen.getByLabelText(/search/i), {
      target: { value: "brazil" },
    });

    await waitFor(() => {
      expect(screen.queryByText(/AgentOffline/)).toBeNull();
    });
    expect(screen.getByText(/PathPacketLoss/)).toBeInTheDocument();
  });

  it("shows empty state when there are no active alerts", async () => {
    mockFetchSequence([{ url: /\/api\/alerts/, status: 200, body: [] }]);
    renderWithProviders(<Alerts />);
    await screen.findByText(/no active alerts/i);
  });

  it("treats 503 from alerts proxy as empty list (alertmanager unreachable)", async () => {
    mockFetchSequence([{ url: /\/api\/alerts/, status: 503, body: "" }]);
    renderWithProviders(<Alerts />);
    await screen.findByText(/no active alerts/i);
  });

  it("filters by protocol via dropdown", async () => {
    mockFetchSequence([
      {
        url: /\/api\/alerts/,
        status: 200,
        body: [
          {
            fingerprint: "a",
            starts_at: new Date(Date.now() - 60_000).toISOString(),
            ends_at: "0001-01-01T00:00:00Z",
            state: "active",
            labels: {
              alertname: "PathPacketLoss",
              severity: "critical",
              category: "loss",
              protocol: "icmp",
            },
            summary: "loss on a (icmp)",
            description: null,
          },
          {
            fingerprint: "b",
            starts_at: new Date(Date.now() - 60_000).toISOString(),
            ends_at: "0001-01-01T00:00:00Z",
            state: "active",
            labels: {
              alertname: "PathLatencyRegression",
              severity: "warning",
              category: "latency",
              protocol: "tcp",
            },
            summary: "latency on b (tcp)",
            description: null,
          },
        ],
      },
    ]);

    renderWithProviders(<Alerts />);
    await screen.findByText(/PathPacketLoss/);
    expect(screen.getByText(/PathLatencyRegression/)).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox", { name: /protocol/i }));
    await user.click(await screen.findByRole("option", { name: /^icmp$/i }));

    await waitFor(() => {
      expect(screen.queryByText(/PathLatencyRegression/)).toBeNull();
    });
    expect(screen.getByText(/PathPacketLoss/)).toBeInTheDocument();
  });
});
