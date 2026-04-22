import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { type ReactElement, type ReactNode, useEffect } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import { AgentsTable } from "@/components/AgentsTable";
import { useIpHostnameContext } from "@/components/ip-hostname/IpHostnameProvider";
import { renderWithProviders } from "@/test/query-wrapper";

// Jsdom stand-in for the provider's lazy `EventSource`. Tests don't emit;
// seeded fixtures exercise the resolved-path, and cold-miss tests rely on the
// provider's natural absent-key state.
class MockEventSource {
  static instances: MockEventSource[] = [];
  listeners: Record<string, Array<(event: { data: string }) => void>> = {};
  constructor(public url: string) {
    MockEventSource.instances.push(this);
  }
  addEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name] ?? [];
    list.push(handler);
    this.listeners[name] = list;
  }
  removeEventListener(name: string, handler: (event: { data: string }) => void): void {
    const list = this.listeners[name];
    if (!list) return;
    const idx = list.indexOf(handler);
    if (idx >= 0) list.splice(idx, 1);
  }
  close(): void {}
}

interface SeedEntry {
  ip: string;
  hostname?: string | null;
}

/**
 * Seeds the shared `IpHostnameProvider` with a fixture set before first
 * paint. Runs inside a mount-only `useEffect` so React 19 strict mode
 * doesn't trip on a setState during render; RTL flushes effects before
 * returning from `render`, so assertions immediately afterwards see the
 * seeded value.
 */
function Seeder({ seed, children }: { seed: SeedEntry[]; children: ReactNode }) {
  const { seedFromResponse } = useIpHostnameContext();
  // biome-ignore lint/correctness/useExhaustiveDependencies: mount-only seed
  useEffect(() => {
    if (seed.length > 0) seedFromResponse(seed);
  }, []);
  return <>{children}</>;
}

function renderWithSeed(ui: ReactElement, seed: SeedEntry[] = []) {
  return renderWithProviders(<Seeder seed={seed}>{ui}</Seeder>);
}

const navigate = vi.fn();
vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return { ...actual, useNavigate: () => navigate };
});

// Mock date-fns so tests are deterministic regardless of wall-clock time.
vi.mock("date-fns", async (importOriginal) => {
  const actual = await importOriginal<typeof import("date-fns")>();
  return {
    ...actual,
    formatDistanceToNowStrict: (_date: Date, _opts?: unknown) => "1 minute ago",
  };
});

// Use a timestamp far in the future so isStale() always returns false (agents are online).
const FUTURE_SEEN = new Date(Date.now() + 60_000).toISOString();

const AGENTS: AgentSummary[] = [
  {
    id: "zeta",
    display_name: "Zeta",
    ip: "10.0.0.26",
    location: "Osaka",
    agent_version: "0.1.0",
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: FUTURE_SEEN,
  },
  {
    id: "alpha",
    display_name: "Alpha",
    ip: "10.0.0.1",
    location: "Frankfurt",
    agent_version: "0.1.0",
    registered_at: "2026-01-01T00:00:00Z",
    last_seen_at: FUTURE_SEEN,
  },
];

beforeEach(() => {
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
});

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("AgentsTable", () => {
  describe("column rendering", () => {
    test("renders all column headers", async () => {
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      expect(await screen.findByRole("columnheader", { name: "ID" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Name" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Location" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "IP" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Hostname" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Version" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: /Last seen/i })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Status" })).toBeInTheDocument();
    });

    test("renders row data for all agents", async () => {
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      // IDs
      expect(await screen.findByText("zeta")).toBeInTheDocument();
      expect(screen.getByText("alpha")).toBeInTheDocument();

      // Names
      expect(screen.getByText("Zeta")).toBeInTheDocument();
      expect(screen.getByText("Alpha")).toBeInTheDocument();

      // Locations
      expect(screen.getByText("Osaka")).toBeInTheDocument();
      expect(screen.getByText("Frankfurt")).toBeInTheDocument();

      // IPs — rendered in both the IP column (plain) and the Hostname
      // column (via <IpHostname>, which falls back to a bare IP on a cold
      // miss). Two matches per IP is the expected shape.
      expect(screen.getAllByText("10.0.0.26")).toHaveLength(2);
      expect(screen.getAllByText("10.0.0.1")).toHaveLength(2);

      // Versions
      const versions = screen.getAllByText("0.1.0");
      expect(versions.length).toBeGreaterThanOrEqual(2);

      // Relative timestamps (mocked date-fns)
      const timestamps = screen.getAllByText("1 minute ago");
      expect(timestamps.length).toBeGreaterThanOrEqual(2);

      // Status badges (both agents are recent so "Online")
      const statusBadges = screen.getAllByText("Online");
      expect(statusBadges.length).toBeGreaterThanOrEqual(2);
    });
  });

  describe("sorting", () => {
    test("clicking Name header sorts rows ascending by display_name", async () => {
      const user = userEvent.setup();
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      const nameHeader = await screen.findByRole("columnheader", { name: "Name" });

      // Initially: zeta first (insertion order), alpha second
      expect(screen.getByText("zeta")).toBeInTheDocument();

      // Click to sort ascending
      await user.click(nameHeader);

      // After ascending sort: Alpha before Zeta.
      // Data rows expose role="link" (full-row navigation); header row keeps implicit role="row".
      const dataRows = screen.getAllByRole("link");
      expect(dataRows[0]).toHaveTextContent("alpha");
      expect(dataRows[1]).toHaveTextContent("zeta");

      // Click again to sort descending
      await user.click(nameHeader);

      const dataRowsDesc = screen.getAllByRole("link");
      expect(dataRowsDesc[0]).toHaveTextContent("zeta");
      expect(dataRowsDesc[1]).toHaveTextContent("alpha");
    });
  });

  describe("filtering", () => {
    test("typing in filter input narrows rows by id", async () => {
      const user = userEvent.setup();
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      const filterInput = await screen.findByRole("textbox", { name: /filter agents/i });

      await user.type(filterInput, "alpha");

      // Only alpha row should remain
      expect(screen.getByText("alpha")).toBeInTheDocument();
      expect(screen.queryByText("zeta")).not.toBeInTheDocument();
    });

    test("typing in filter input narrows rows by display_name", async () => {
      const user = userEvent.setup();
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      const filterInput = await screen.findByRole("textbox", { name: /filter agents/i });

      await user.type(filterInput, "Zeta");

      // Only zeta row should remain
      expect(screen.getByText("Zeta")).toBeInTheDocument();
      expect(screen.queryByText("Alpha")).not.toBeInTheDocument();
    });

    test("filter is case-insensitive", async () => {
      const user = userEvent.setup();
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      const filterInput = await screen.findByRole("textbox", { name: /filter agents/i });

      await user.type(filterInput, "ALPHA");

      expect(screen.getByText("alpha")).toBeInTheDocument();
      expect(screen.queryByText("zeta")).not.toBeInTheDocument();
    });
  });

  describe("row navigation", () => {
    test("clicking the ID cell navigates to /agents/$id", async () => {
      const user = userEvent.setup();
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      await screen.findByText("zeta");
      navigate.mockClear();

      await user.click(screen.getByText("zeta"));

      expect(navigate).toHaveBeenCalledWith({
        to: "/agents/$id",
        params: { id: "zeta" },
      });
    });

    test("clicking a non-ID cell (Name) also navigates", async () => {
      const user = userEvent.setup();
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      await screen.findByText("Alpha");
      navigate.mockClear();

      await user.click(screen.getByText("Alpha"));

      expect(navigate).toHaveBeenCalledWith({
        to: "/agents/$id",
        params: { id: "alpha" },
      });
    });

    test("pressing Enter on a focused row navigates", async () => {
      const user = userEvent.setup();
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      await screen.findByText("zeta");
      navigate.mockClear();

      const row = screen.getByRole("link", { name: /Open agent zeta/i });
      row.focus();
      await user.keyboard("{Enter}");

      expect(navigate).toHaveBeenCalledWith({
        to: "/agents/$id",
        params: { id: "zeta" },
      });
    });

    test("rows expose link role, tabIndex, and aria-label for accessibility", async () => {
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      await screen.findByText("zeta");

      const zetaRow = screen.getByRole("link", { name: /Open agent zeta/i });
      expect(zetaRow).toHaveAttribute("tabindex", "0");

      const alphaRow = screen.getByRole("link", { name: /Open agent alpha/i });
      expect(alphaRow).toHaveAttribute("tabindex", "0");
    });

    test("clicking the Hostname cell still bubbles to the row-level navigation", async () => {
      const user = userEvent.setup();
      renderWithSeed(<AgentsTable agents={AGENTS} />, [
        { ip: "10.0.0.1", hostname: "alpha.example.com" },
      ]);

      // Wait for the seeded hostname to paint, then click directly on the
      // hostname-bearing cell. The row's onClick must still fire because
      // the IpHostname cell does not stop propagation.
      const hostnameCell = await screen.findByText("(alpha.example.com)");
      navigate.mockClear();
      await user.click(hostnameCell);

      expect(navigate).toHaveBeenCalledWith({
        to: "/agents/$id",
        params: { id: "alpha" },
      });
    });
  });

  describe("hostname column", () => {
    test("renders `ip (hostname)` in the Hostname cell when the provider has a positive hit", async () => {
      renderWithSeed(<AgentsTable agents={AGENTS} />, [
        { ip: "10.0.0.1", hostname: "alpha.example.com" },
      ]);

      // Seeder effect primes the provider map; the cell re-renders with
      // the muted parenthesised hostname once the value lands.
      expect(await screen.findByText("(alpha.example.com)")).toBeInTheDocument();
      // Screen-reader companion is a single combined phrase.
      expect(screen.getByText("10.0.0.1, hostname alpha.example.com")).toBeInTheDocument();
    });

    test("renders bare IP in the Hostname cell on a cold miss (no seed)", async () => {
      renderWithSeed(<AgentsTable agents={AGENTS} />, []);

      // Cold-miss: the IP column renders the plain IP and the Hostname
      // column falls back to the bare IP as well — two matches total.
      const ips = await screen.findAllByText("10.0.0.1");
      expect(ips).toHaveLength(2);
      // No hostname suffix is emitted when the map has no entry.
      expect(screen.queryByText(/hostname/)).not.toBeInTheDocument();
    });
  });
});
