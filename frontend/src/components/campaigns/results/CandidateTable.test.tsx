import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { Evaluation } from "@/api/hooks/evaluation";
import {
  CandidateTable,
  type CandidateTableSort,
} from "@/components/campaigns/results/CandidateTable";

afterEach(() => cleanup());

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

type Candidate = Evaluation["results"]["candidates"][number];

function makeCandidate(overrides: Partial<Candidate> & { destination_ip: string }): Candidate {
  return {
    destination_ip: overrides.destination_ip,
    display_name: overrides.display_name ?? null,
    city: overrides.city ?? null,
    country_code: overrides.country_code ?? null,
    asn: overrides.asn ?? null,
    network_operator: overrides.network_operator ?? null,
    is_mesh_member: overrides.is_mesh_member ?? false,
    pairs_improved: overrides.pairs_improved ?? 0,
    pairs_total_considered: overrides.pairs_total_considered ?? 3,
    avg_improvement_ms: overrides.avg_improvement_ms ?? null,
    avg_loss_pct: overrides.avg_loss_pct ?? null,
    composite_score: overrides.composite_score ?? 0,
    pair_details: overrides.pair_details ?? [],
  };
}

function makeEvaluation(candidates: Candidate[], overrides?: Partial<Evaluation>): Evaluation {
  return {
    campaign_id: overrides?.campaign_id ?? "cccccccc-cccc-cccc-cccc-cccccccccccc",
    evaluated_at: overrides?.evaluated_at ?? "2026-04-21T10:00:00Z",
    loss_threshold_pct: overrides?.loss_threshold_pct ?? 2,
    stddev_weight: overrides?.stddev_weight ?? 1,
    evaluation_mode: overrides?.evaluation_mode ?? "optimization",
    baseline_pair_count: overrides?.baseline_pair_count ?? 6,
    candidates_total: overrides?.candidates_total ?? candidates.length,
    candidates_good: overrides?.candidates_good ?? 0,
    avg_improvement_ms: overrides?.avg_improvement_ms ?? null,
    results: {
      candidates,
      unqualified_reasons: overrides?.results?.unqualified_reasons ?? {},
    },
  };
}

const DEFAULT_SORT: CandidateTableSort = { col: "composite_score", dir: "desc" };

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("CandidateTable", () => {
  test("renders the summary KPI strip with evaluation totals", () => {
    const evaluation = makeEvaluation(
      [
        makeCandidate({ destination_ip: "10.0.0.1", composite_score: 1 }),
        makeCandidate({ destination_ip: "10.0.0.2", composite_score: 2 }),
      ],
      {
        baseline_pair_count: 7,
        candidates_total: 5,
        candidates_good: 2,
        avg_improvement_ms: 42,
      },
    );

    render(
      <CandidateTable
        evaluation={evaluation}
        onSelectCandidate={() => {}}
        sort={DEFAULT_SORT}
        onSortChange={() => {}}
      />,
    );

    expect(screen.getByText("Baseline pairs")).toBeInTheDocument();
    expect(screen.getByText("7")).toBeInTheDocument();
    expect(screen.getByText("2 / 5")).toBeInTheDocument();
    // Positive improvement renders with a leading "+" and the ms suffix.
    expect(screen.getByText("+42.0 ms")).toBeInTheDocument();
  });

  test("renders positive improvement in green and negative in red", () => {
    const evaluation = makeEvaluation([
      makeCandidate({
        destination_ip: "10.0.0.1",
        display_name: "alpha",
        avg_improvement_ms: 57,
        is_mesh_member: true,
        composite_score: 5,
      }),
      makeCandidate({
        destination_ip: "10.0.0.2",
        display_name: "beta",
        avg_improvement_ms: -12,
        composite_score: 1,
      }),
    ]);

    render(
      <CandidateTable
        evaluation={evaluation}
        onSelectCandidate={() => {}}
        sort={DEFAULT_SORT}
        onSortChange={() => {}}
      />,
    );

    const positive = screen.getByText("+57.0 ms");
    const negative = screen.getByText("-12.0 ms");
    expect(positive.className).toMatch(/emerald/);
    expect(negative.className).toMatch(/destructive/);

    // Mesh badge only on the first candidate.
    expect(screen.getByLabelText(/mesh member/i)).toBeInTheDocument();
  });

  test("renders the Unknown ASN chip when catalogue has no AS number", () => {
    const evaluation = makeEvaluation([
      makeCandidate({ destination_ip: "10.0.0.1", composite_score: 1 }),
    ]);

    render(
      <CandidateTable
        evaluation={evaluation}
        onSelectCandidate={() => {}}
        sort={DEFAULT_SORT}
        onSortChange={() => {}}
      />,
    );

    expect(screen.getByText("Unknown")).toBeInTheDocument();
  });

  test("clicking a row fires onSelectCandidate with the destination IP", () => {
    const onSelect = vi.fn();
    const evaluation = makeEvaluation([
      makeCandidate({ destination_ip: "10.0.0.9", composite_score: 3 }),
    ]);

    render(
      <CandidateTable
        evaluation={evaluation}
        onSelectCandidate={onSelect}
        sort={DEFAULT_SORT}
        onSortChange={() => {}}
      />,
    );

    fireEvent.click(screen.getByTestId("candidate-row-10.0.0.9"));
    expect(onSelect).toHaveBeenCalledWith("10.0.0.9");
  });

  test("clicking a sortable header fires onSortChange with the next direction", () => {
    const onSortChange = vi.fn();
    const evaluation = makeEvaluation([
      makeCandidate({ destination_ip: "10.0.0.1", composite_score: 1 }),
      makeCandidate({ destination_ip: "10.0.0.2", composite_score: 2 }),
    ]);

    render(
      <CandidateTable
        evaluation={evaluation}
        onSelectCandidate={() => {}}
        sort={{ col: "composite_score", dir: "desc" }}
        onSortChange={onSortChange}
      />,
    );

    // First click on the active column flips to ascending.
    fireEvent.click(screen.getByRole("button", { name: /score/i }));
    expect(onSortChange).toHaveBeenCalledWith({ col: "composite_score", dir: "asc" });
  });

  test("sorts rows by the configured column and direction", () => {
    const evaluation = makeEvaluation([
      makeCandidate({ destination_ip: "10.0.0.1", composite_score: 1.5 }),
      makeCandidate({ destination_ip: "10.0.0.2", composite_score: 9 }),
      makeCandidate({ destination_ip: "10.0.0.3", composite_score: 4.5 }),
    ]);

    const { rerender } = render(
      <CandidateTable
        evaluation={evaluation}
        onSelectCandidate={() => {}}
        sort={{ col: "composite_score", dir: "desc" }}
        onSortChange={() => {}}
      />,
    );

    let rowIps = screen
      .getAllByTestId(/candidate-row-/)
      .map((row) => row.getAttribute("data-testid"));
    expect(rowIps).toEqual([
      "candidate-row-10.0.0.2",
      "candidate-row-10.0.0.3",
      "candidate-row-10.0.0.1",
    ]);

    rerender(
      <CandidateTable
        evaluation={evaluation}
        onSelectCandidate={() => {}}
        sort={{ col: "composite_score", dir: "asc" }}
        onSortChange={() => {}}
      />,
    );

    rowIps = screen.getAllByTestId(/candidate-row-/).map((row) => row.getAttribute("data-testid"));
    expect(rowIps).toEqual([
      "candidate-row-10.0.0.1",
      "candidate-row-10.0.0.3",
      "candidate-row-10.0.0.2",
    ]);
  });

  test("renders row-action slot when renderRowActions is supplied", () => {
    const evaluation = makeEvaluation([
      makeCandidate({ destination_ip: "10.0.0.1", composite_score: 1 }),
    ]);

    render(
      <CandidateTable
        evaluation={evaluation}
        onSelectCandidate={() => {}}
        sort={DEFAULT_SORT}
        onSortChange={() => {}}
        renderRowActions={(c) => (
          <button type="button" aria-label={`actions-${c.destination_ip}`}>
            actions
          </button>
        )}
      />,
    );

    expect(screen.getByLabelText("actions-10.0.0.1")).toBeInTheDocument();
  });

  test("renders the empty state when no candidates are present", () => {
    const evaluation = makeEvaluation([]);

    render(
      <CandidateTable
        evaluation={evaluation}
        onSelectCandidate={() => {}}
        sort={DEFAULT_SORT}
        onSortChange={() => {}}
      />,
    );

    expect(screen.getByText(/no candidates matched/i)).toBeInTheDocument();
  });
});
