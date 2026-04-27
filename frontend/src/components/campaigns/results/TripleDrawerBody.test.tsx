/**
 * TripleDrawerBody tests.
 *
 * Focus areas:
 * - winning_x_position chip renders only when non-null (M1 requirement)
 * - chip text is correct for position 1 and 2
 * - chip is absent when winning_x_position is null/undefined
 */

import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { Campaign } from "@/api/hooks/campaigns";
import type { Evaluation } from "@/api/hooks/evaluation";
import type {
  EvaluationPairDetailListResponse,
  PairDetailsQuery,
} from "@/api/hooks/evaluation-pairs";
import { IpHostnameProvider } from "@/components/ip-hostname";

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

vi.mock("@/api/hooks/evaluation-pairs", async () => {
  const actual = await vi.importActual<typeof import("@/api/hooks/evaluation-pairs")>(
    "@/api/hooks/evaluation-pairs",
  );
  return { ...actual, useCandidatePairDetails: vi.fn() };
});

vi.mock("@/components/RouteTopology", () => ({
  RouteTopology: () => <div data-testid="route-topology" />,
}));

import { useAgents } from "@/api/hooks/agents";
import { useCampaignMeasurements } from "@/api/hooks/campaigns";
import { useCandidatePairDetails } from "@/api/hooks/evaluation-pairs";
import { TripleDrawerBody } from "@/components/campaigns/results/TripleDrawerBody";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CAMPAIGN_ID = "33333333-3333-3333-3333-333333333333";
const CANDIDATE_IP = "10.0.88.1";

function makeCampaign(): Campaign {
  return {
    id: CAMPAIGN_ID,
    title: "t",
    notes: "",
    state: "evaluated",
    protocol: "icmp",
    evaluation_mode: "optimization",
    force_measurement: false,
    loss_threshold_ratio: 0.02,
    stddev_weight: 1,
    probe_count: 10,
    probe_count_detail: 250,
    probe_stagger_ms: 100,
    timeout_ms: 2000,
    created_at: "2026-04-01T12:00:00Z",
    created_by: "alice",
    started_at: null,
    stopped_at: null,
    completed_at: null,
    evaluated_at: null,
    pair_counts: [["succeeded", 6]],
  } as unknown as Campaign;
}

function makeCandidate(
  overrides: Partial<Evaluation["results"]["candidates"][number]> = {},
): Evaluation["results"]["candidates"][number] {
  return {
    destination_ip: CANDIDATE_IP,
    display_name: overrides.display_name ?? "transit-x",
    city: null,
    country_code: null,
    asn: null,
    network_operator: null,
    is_mesh_member: false,
    pairs_improved: overrides.pairs_improved ?? 5,
    pairs_total_considered: overrides.pairs_total_considered ?? 100,
    avg_improvement_ms: 22,
    avg_loss_ratio: 0.0005,
    composite_score: 10,
    hostname: null,
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

function makeEntry(idx: number, winning_x_position?: number | null): Record<string, unknown> {
  return {
    source_agent_id: `agent-${idx}-src`,
    destination_agent_id: `agent-${idx}-dst`,
    destination_ip: CANDIDATE_IP,
    direct_rtt_ms: 50,
    direct_stddev_ms: 1,
    direct_loss_ratio: 0.001,
    direct_source: "active_probe",
    transit_rtt_ms: 30,
    transit_stddev_ms: 0.5,
    transit_loss_ratio: 0.0005,
    improvement_ms: 20,
    qualifies: true,
    winning_x_position: winning_x_position ?? null,
  };
}

function pageOf(
  entries: ReturnType<typeof makeEntry>[],
  total: number,
): EvaluationPairDetailListResponse {
  return {
    entries: entries as EvaluationPairDetailListResponse["entries"],
    next_cursor: null,
    total,
  };
}

interface PairsHookReturn {
  data?: { pages: EvaluationPairDetailListResponse[]; pageParams: Array<string | null> };
  isLoading: boolean;
  isError: boolean;
  isFetchingNextPage: boolean;
  hasNextPage: boolean;
  error: Error | null;
  fetchNextPage: ReturnType<typeof vi.fn>;
  refetch: ReturnType<typeof vi.fn>;
}

function pairsReturn(overrides: Partial<PairsHookReturn> = {}): PairsHookReturn {
  return {
    data: overrides.data ?? {
      pages: [pageOf([], 0)],
      pageParams: [null],
    },
    isLoading: overrides.isLoading ?? false,
    isError: overrides.isError ?? false,
    isFetchingNextPage: overrides.isFetchingNextPage ?? false,
    hasNextPage: overrides.hasNextPage ?? false,
    error: overrides.error ?? null,
    fetchNextPage: overrides.fetchNextPage ?? vi.fn(),
    refetch: overrides.refetch ?? vi.fn(),
  };
}

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

function renderBody(
  filteredHook: PairsHookReturn = pairsReturn(),
  unfilteredHook: PairsHookReturn = pairsReturn(),
  candidate = makeCandidate(),
) {
  vi.mocked(useCandidatePairDetails).mockImplementation((_id, _ip, q: PairDetailsQuery) => {
    const r = q.limit === 0 ? unfilteredHook : filteredHook;
    return r as unknown as ReturnType<typeof useCandidatePairDetails>;
  });

  vi.mocked(useAgents).mockReturnValue({
    data: [
      makeAgent("agent-1-src", "alpha", "10.0.0.1"),
      makeAgent("agent-1-dst", "beta", "10.0.0.2"),
    ],
    isLoading: false,
    isError: false,
  } as unknown as ReturnType<typeof useAgents>);

  vi.mocked(useCampaignMeasurements).mockReturnValue({
    data: { pages: [{ entries: [], next_cursor: null }], pageParams: [null] },
    isLoading: false,
    isError: false,
    error: null,
  } as unknown as ReturnType<typeof useCampaignMeasurements>);

  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });

  function Wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={client}>
        <IpHostnameProvider>{children}</IpHostnameProvider>
      </QueryClientProvider>
    );
  }

  return render(
    <TripleDrawerBody
      candidate={candidate}
      campaign={makeCampaign()}
      evaluation={null}
      unqualifiedReason={undefined}
      onClose={vi.fn()}
    />,
    { wrapper: Wrapper },
  );
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("TripleDrawerBody — winning_x_position chip", () => {
  test("renders 'X first' chip when winning_x_position === 1", () => {
    const entry = makeEntry(1, 1);
    renderBody(
      pairsReturn({ data: { pages: [pageOf([entry], 1)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 1)], pageParams: [null] } }),
    );
    expect(screen.getByTestId("winning-x-position-0")).toBeInTheDocument();
    expect(screen.getByText(/X first \(A → X → Y → B\)/)).toBeInTheDocument();
  });

  test("renders 'X second' chip when winning_x_position === 2", () => {
    const entry = makeEntry(1, 2);
    renderBody(
      pairsReturn({ data: { pages: [pageOf([entry], 1)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 1)], pageParams: [null] } }),
    );
    expect(screen.getByTestId("winning-x-position-0")).toBeInTheDocument();
    expect(screen.getByText(/X second \(A → Y → X → B\)/)).toBeInTheDocument();
  });

  test("does not render any winning-x-position chip when winning_x_position is null", () => {
    const entry = makeEntry(1, null);
    renderBody(
      pairsReturn({ data: { pages: [pageOf([entry], 1)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 1)], pageParams: [null] } }),
    );
    expect(screen.queryByTestId("winning-x-position-0")).not.toBeInTheDocument();
    expect(screen.queryByText(/X first/)).not.toBeInTheDocument();
    expect(screen.queryByText(/X second/)).not.toBeInTheDocument();
  });

  test("does not render any winning-x-position chip when winning_x_position is undefined", () => {
    const entry = makeEntry(1, undefined);
    renderBody(
      pairsReturn({ data: { pages: [pageOf([entry], 1)], pageParams: [null] } }),
      pairsReturn({ data: { pages: [pageOf([], 1)], pageParams: [null] } }),
    );
    expect(screen.queryByTestId("winning-x-position-0")).not.toBeInTheDocument();
  });

  test("renders the drilldown body container", () => {
    renderBody();
    expect(screen.getByTestId("drilldown-body")).toBeInTheDocument();
  });
});
