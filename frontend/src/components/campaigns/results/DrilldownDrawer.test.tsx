import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { Campaign } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";

// ---------------------------------------------------------------------------
// Module mocks
// ---------------------------------------------------------------------------

vi.mock("@/api/hooks/agents", async () => {
  const actual = await vi.importActual<typeof import("@/api/hooks/agents")>("@/api/hooks/agents");
  return { ...actual, useAgents: vi.fn() };
});

vi.mock("@/api/hooks/campaigns", async () => {
  const actual =
    await vi.importActual<typeof import("@/api/hooks/campaigns")>("@/api/hooks/campaigns");
  return { ...actual, useCampaignMeasurements: vi.fn() };
});

// `RouteTopology` uses cytoscape, which pokes the DOM in ways jsdom struggles
// with. Swap it for a visible stub so the test asserts on "MTR rendered" not
// on graph-engine internals.
vi.mock("@/components/RouteTopology", () => ({
  RouteTopology: ({ hops, ariaLabel }: { hops: unknown[]; ariaLabel?: string }) => (
    <div data-testid="route-topology" aria-label={ariaLabel}>
      hops: {hops.length}
    </div>
  ),
}));

import { useAgents } from "@/api/hooks/agents";
import { useCampaignMeasurements } from "@/api/hooks/campaigns";
import { DrilldownDrawer } from "@/components/campaigns/results/DrilldownDrawer";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

type Candidate = Evaluation["results"]["candidates"][number];
type PairDetail = Candidate["pair_details"][number];

function makePair(overrides: Partial<PairDetail> = {}): PairDetail {
  return {
    source_agent_id: overrides.source_agent_id ?? "agent-a",
    destination_agent_id: overrides.destination_agent_id ?? "agent-b",
    destination_ip: overrides.destination_ip ?? "10.0.99.1",
    direct_rtt_ms: overrides.direct_rtt_ms ?? 42,
    direct_stddev_ms: overrides.direct_stddev_ms ?? 1,
    direct_loss_pct: overrides.direct_loss_pct ?? 0.1,
    transit_rtt_ms: overrides.transit_rtt_ms ?? 20,
    transit_stddev_ms: overrides.transit_stddev_ms ?? 0.5,
    transit_loss_pct: overrides.transit_loss_pct ?? 0.05,
    improvement_ms: overrides.improvement_ms ?? 22,
    qualifies: overrides.qualifies ?? true,
    mtr_measurement_id_ax: overrides.mtr_measurement_id_ax ?? null,
    mtr_measurement_id_xb: overrides.mtr_measurement_id_xb ?? null,
  };
}

function makeCandidate(overrides: Partial<Candidate> = {}): Candidate {
  return {
    destination_ip: overrides.destination_ip ?? "10.0.99.1",
    display_name: overrides.display_name ?? "transit-x",
    city: overrides.city ?? null,
    country_code: overrides.country_code ?? null,
    asn: overrides.asn ?? null,
    network_operator: overrides.network_operator ?? null,
    is_mesh_member: overrides.is_mesh_member ?? false,
    pairs_improved: overrides.pairs_improved ?? 1,
    pairs_total_considered: overrides.pairs_total_considered ?? 1,
    avg_improvement_ms: overrides.avg_improvement_ms ?? 22,
    avg_loss_pct: overrides.avg_loss_pct ?? 0.05,
    composite_score: overrides.composite_score ?? 10,
    pair_details: overrides.pair_details ?? [makePair()],
  };
}

function makeAgent(id: string, display_name: string, ip: string): AgentSummary {
  return {
    id,
    display_name,
    ip,
    last_seen_at: "2026-04-21T10:00:00Z",
    registered_at: "2026-04-01T10:00:00Z",
  };
}

const CAMPAIGN: Campaign = {
  id: "cccccccc-cccc-cccc-cccc-cccccccccccc",
  title: "Demo",
  notes: null,
  state: "evaluated",
  protocol: "icmp",
  evaluation_mode: "optimization",
  force_measurement: false,
  loss_threshold_pct: 2,
  stddev_weight: 1,
  probe_count: 10,
  probe_count_detail: 250,
  probe_stagger_ms: 100,
  timeout_ms: 2000,
  created_at: "2026-04-01T10:00:00Z",
  created_by: "alice",
  started_at: null,
  stopped_at: null,
  completed_at: null,
  evaluated_at: null,
  pair_counts: [],
};

// ---------------------------------------------------------------------------
// Mock wiring
// ---------------------------------------------------------------------------

function wireAgents(agents: AgentSummary[]): void {
  vi.mocked(useAgents).mockReturnValue({
    data: agents,
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useAgents>);
}

function wireMeasurements(entry: unknown | null, opts?: { isLoading?: boolean; isError?: boolean }) {
  vi.mocked(useCampaignMeasurements).mockReturnValue({
    data: entry
      ? { pages: [{ entries: [entry], next_cursor: null }], pageParams: [null] }
      : { pages: [{ entries: [], next_cursor: null }], pageParams: [null] },
    isLoading: opts?.isLoading ?? false,
    isError: opts?.isError ?? false,
    error: opts?.isError ? new Error("boom") : null,
  } as unknown as ReturnType<typeof useCampaignMeasurements>);
}

beforeEach(() => {
  wireAgents([
    makeAgent("agent-a", "alpha", "10.0.0.1"),
    makeAgent("agent-b", "beta", "10.0.0.2"),
  ]);
  wireMeasurements(null);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("DrilldownDrawer", () => {
  test("is closed when candidate is null", () => {
    render(<DrilldownDrawer candidate={null} campaign={CAMPAIGN} onClose={() => {}} />);
    expect(screen.queryByText(/transit-x/)).not.toBeInTheDocument();
  });

  test("renders pair detail rows with source+destination labels resolved via agent roster", () => {
    const candidate = makeCandidate({
      pair_details: [makePair({ source_agent_id: "agent-a", destination_agent_id: "agent-b" })],
    });

    render(<DrilldownDrawer candidate={candidate} campaign={CAMPAIGN} onClose={() => {}} />);

    // Source and destination display names come from the agent roster, not
    // from the pair row's `destination_ip`.
    expect(screen.getByText("alpha")).toBeInTheDocument();
    expect(screen.getByText("beta")).toBeInTheDocument();
    // The dest IP shown next to the labels should be the agent roster's IP
    // (10.0.0.2), not the transit-X IP (10.0.99.1).
    expect(screen.getByText("(10.0.0.2)")).toBeInTheDocument();
  });

  test("renders positive improvement green, negative red", () => {
    const candidate = makeCandidate({
      pair_details: [
        makePair({ improvement_ms: 57, destination_agent_id: "agent-b" }),
        makePair({ improvement_ms: -10, destination_agent_id: "agent-a" }),
      ],
    });

    render(<DrilldownDrawer candidate={candidate} campaign={CAMPAIGN} onClose={() => {}} />);

    expect(screen.getByText("+57.0 ms").className).toMatch(/emerald/);
    expect(screen.getByText("-10.0 ms").className).toMatch(/destructive/);
  });

  test("MTR link is disabled when measurement_id is null", () => {
    const candidate = makeCandidate({
      pair_details: [makePair({ mtr_measurement_id_ax: null, mtr_measurement_id_xb: null })],
    });

    render(<DrilldownDrawer candidate={candidate} campaign={CAMPAIGN} onClose={() => {}} />);

    const disabled = screen.getAllByRole("button", { name: /unavailable/i });
    expect(disabled.length).toBeGreaterThan(0);
    for (const btn of disabled) {
      expect(btn).toBeDisabled();
    }
  });

  test("clicking an MTR link renders the RouteTopology stub from the fetched hops", () => {
    const candidate = makeCandidate({
      pair_details: [makePair({ mtr_measurement_id_ax: 42 })],
    });

    wireMeasurements({
      pair_id: 1,
      source_agent_id: "agent-a",
      destination_ip: "10.0.99.1",
      resolution_state: "succeeded",
      pair_kind: "detail_mtr",
      measurement_id: 42,
      mtr_id: 7,
      mtr_hops: [
        {
          position: 0,
          observed_ips: [{ ip: "10.0.0.1", freq: 1 }],
          avg_rtt_micros: 1000,
          stddev_rtt_micros: 10,
          loss_pct: 0,
        },
      ],
      protocol: "icmp",
    });

    render(<DrilldownDrawer candidate={candidate} campaign={CAMPAIGN} onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: /MTR alpha → 10\.0\.99\.1/i }));
    expect(screen.getByTestId("route-topology")).toBeInTheDocument();
    expect(screen.getByText("hops: 1")).toBeInTheDocument();
  });

  test("shows a 'not settled yet' message when the MTR row is absent", () => {
    const candidate = makeCandidate({
      pair_details: [makePair({ mtr_measurement_id_ax: 99 })],
    });

    // no entries in the page → row is undefined
    wireMeasurements(null);

    render(<DrilldownDrawer candidate={candidate} campaign={CAMPAIGN} onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: /MTR alpha/i }));
    expect(screen.getByText(/has not settled yet/i)).toBeInTheDocument();
  });

  test("shows a loading affordance while the measurement query is pending", () => {
    const candidate = makeCandidate({
      pair_details: [makePair({ mtr_measurement_id_ax: 99 })],
    });

    wireMeasurements(null, { isLoading: true });

    render(<DrilldownDrawer candidate={candidate} campaign={CAMPAIGN} onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: /MTR alpha/i }));
    expect(screen.getByText(/loading mtr hops/i)).toBeInTheDocument();
  });

  test("renders unqualified reason when provided", () => {
    const candidate = makeCandidate({ pair_details: [] });

    render(
      <DrilldownDrawer
        candidate={candidate}
        campaign={CAMPAIGN}
        onClose={() => {}}
        unqualifiedReason="Direct A→B already beats every transit."
      />,
    );

    expect(screen.getByText(/Direct A→B already beats every transit/)).toBeInTheDocument();
  });
});
