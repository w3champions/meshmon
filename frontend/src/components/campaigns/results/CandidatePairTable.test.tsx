import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { EvaluationPairDetail } from "@/api/hooks/evaluation-pairs";
import { CandidatePairTable } from "@/components/campaigns/results/CandidatePairTable";
import { IpHostnameProvider } from "@/components/ip-hostname";

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

function makeRow(overrides: Partial<EvaluationPairDetail> = {}): EvaluationPairDetail {
  return {
    source_agent_id: "a",
    destination_agent_id: "b",
    destination_ip: "10.0.99.1",
    direct_rtt_ms: overrides.direct_rtt_ms ?? 50,
    direct_stddev_ms: 1,
    direct_loss_ratio: 0.001,
    direct_source: "active_probe",
    transit_rtt_ms: 30,
    transit_stddev_ms: 0.5,
    transit_loss_ratio: 0.0005,
    improvement_ms: overrides.improvement_ms ?? 20,
    qualifies: true,
    ...overrides,
  } as EvaluationPairDetail;
}

function renderTable(rows: EvaluationPairDetail[]) {
  vi.stubGlobal("EventSource", NoopEventSource);
  const agentsById = new Map<string, AgentSummary>();
  return render(
    <IpHostnameProvider>
      <CandidatePairTable
        rows={rows}
        agentsById={agentsById}
        sort={{ col: "improvement_ms", dir: "desc" }}
        onSortChange={vi.fn()}
        hasNextPage={false}
        isFetchingNextPage={false}
        fetchNextPage={vi.fn()}
        onOpenMtr={vi.fn()}
      />
    </IpHostnameProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("CandidatePairTable — defensive Δ% formatter", () => {
  test("renders '—' when direct_rtt_ms = 0", () => {
    renderTable([makeRow({ direct_rtt_ms: 0, improvement_ms: 5 })]);
    const cell = screen.getByTestId("candidate-pair-row-0-delta-pct");
    expect(cell).toHaveTextContent("—");
  });

  test("renders '—' when improvement_ms is NaN", () => {
    renderTable([makeRow({ improvement_ms: Number.NaN })]);
    const cell = screen.getByTestId("candidate-pair-row-0-delta-pct");
    expect(cell).toHaveTextContent("—");
  });

  test("renders a signed percentage when both inputs are finite", () => {
    renderTable([makeRow({ direct_rtt_ms: 100, improvement_ms: 25 })]);
    const cell = screen.getByTestId("candidate-pair-row-0-delta-pct");
    expect(cell).toHaveTextContent("+25.0 %");
  });
});
