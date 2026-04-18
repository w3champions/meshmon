import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import { AgentsTable } from "@/components/AgentsTable";
import { renderWithProviders } from "@/test/query-wrapper";

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

afterEach(() => {
  vi.clearAllMocks();
});

describe("AgentsTable", () => {
  describe("column rendering", () => {
    test("renders all column headers", async () => {
      renderWithProviders(<AgentsTable agents={AGENTS} />);

      expect(await screen.findByRole("columnheader", { name: "ID" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Name" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "Location" })).toBeInTheDocument();
      expect(screen.getByRole("columnheader", { name: "IP" })).toBeInTheDocument();
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

      // IPs
      expect(screen.getByText("10.0.0.26")).toBeInTheDocument();
      expect(screen.getByText("10.0.0.1")).toBeInTheDocument();

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
  });
});
