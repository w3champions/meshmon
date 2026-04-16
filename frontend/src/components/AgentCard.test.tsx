import { render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { AgentCard } from "@/components/AgentCard";

const FIXED_NOW = new Date("2026-04-16T12:00:00Z");

const AGENT = {
  id: "a1",
  display_name: "Agent One",
  ip: "10.0.0.1",
  location: "Frankfurt",
  agent_version: "0.1.0",
  registered_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-04-16T11:58:00Z", // within 5 min of fixed now → online
};

const STALE_AGENT = {
  ...AGENT,
  id: "a2",
  display_name: "Stale Agent",
  last_seen_at: "2026-04-16T11:50:00Z", // 10 min before fixed now → stale
};

const MINIMAL_AGENT = {
  id: "a3",
  display_name: "Minimal Agent",
  ip: "192.168.1.1",
  registered_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-04-16T11:58:00Z",
};

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(FIXED_NOW);
});

afterEach(() => {
  vi.useRealTimers();
});

describe("AgentCard", () => {
  test("renders display_name and id", () => {
    render(<AgentCard agent={AGENT} />);
    expect(screen.getByText("Agent One")).toBeInTheDocument();
    expect(screen.getByText("a1")).toBeInTheDocument();
  });

  test("renders ip and location when present", () => {
    render(<AgentCard agent={AGENT} />);
    expect(screen.getByText(/10\.0\.0\.1/)).toBeInTheDocument();
    expect(screen.getByText(/Frankfurt/)).toBeInTheDocument();
  });

  test("renders Online badge when agent is fresh", () => {
    render(<AgentCard agent={AGENT} />);
    expect(screen.getByText("Online")).toBeInTheDocument();
  });

  test("renders Stale badge when isStale returns true", () => {
    render(<AgentCard agent={STALE_AGENT} />);
    expect(screen.getByText("Stale")).toBeInTheDocument();
  });

  test("does not render ip or location when absent", () => {
    render(<AgentCard agent={MINIMAL_AGENT} />);
    // No location text
    expect(screen.queryByText(/Frankfurt/)).not.toBeInTheDocument();
  });

  test("compact prop hides the footer row (last-seen + version)", () => {
    render(<AgentCard agent={AGENT} compact />);
    // "ago" appears in the last-seen text — it should not be present in compact mode
    expect(screen.queryByText(/ago/)).not.toBeInTheDocument();
    expect(screen.queryByText(/v0\.1\.0/)).not.toBeInTheDocument();
  });

  test("non-compact shows last-seen and version", () => {
    render(<AgentCard agent={AGENT} />);
    expect(screen.getByText(/ago/)).toBeInTheDocument();
    expect(screen.getByText(/v0\.1\.0/)).toBeInTheDocument();
  });
});
