import "@testing-library/jest-dom/vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { PairDetailsQuery } from "@/api/hooks/evaluation-pairs";
import { CandidatePairFilters } from "@/components/campaigns/results/CandidatePairFilters";

const NO_GUARDRAILS = {
  min_improvement_ms: null,
  min_improvement_ratio: null,
  max_transit_rtt_ms: null,
  max_transit_stddev_ms: null,
};

function makeQuery(overrides: Partial<PairDetailsQuery> = {}): PairDetailsQuery {
  return {
    sort: overrides.sort ?? "improvement_ms",
    dir: overrides.dir ?? "desc",
    min_improvement_ms: overrides.min_improvement_ms ?? null,
    min_improvement_ratio: overrides.min_improvement_ratio ?? null,
    max_transit_rtt_ms: overrides.max_transit_rtt_ms ?? null,
    max_transit_stddev_ms: overrides.max_transit_stddev_ms ?? null,
    qualifies_only: overrides.qualifies_only ?? null,
  };
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});

afterEach(() => {
  vi.useRealTimers();
  cleanup();
});

describe("CandidatePairFilters", () => {
  test("typing a positive number commits after debounce", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(
      <CandidatePairFilters value={makeQuery()} onChange={onChange} guardrails={NO_GUARDRAILS} />,
    );

    await user.type(screen.getByTestId("filter-min-improvement-ms"), "5");
    expect(onChange).not.toHaveBeenCalled();
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ min_improvement_ms: 5 }));
  });

  test("clearing the input commits null", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(
      <CandidatePairFilters
        value={makeQuery({ min_improvement_ms: 5 })}
        onChange={onChange}
        guardrails={NO_GUARDRAILS}
      />,
    );

    const input = screen.getByTestId("filter-min-improvement-ms");
    await user.clear(input);
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(onChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ min_improvement_ms: null }),
    );
  });

  test("typing a negative value round-trips cleanly (signed thresholds match I2)", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(
      <CandidatePairFilters value={makeQuery()} onChange={onChange} guardrails={NO_GUARDRAILS} />,
    );

    await user.type(screen.getByTestId("filter-min-improvement-ms"), "-10");
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ min_improvement_ms: -10 }));
  });

  test("active guardrails render as input placeholders", () => {
    render(
      <CandidatePairFilters
        value={makeQuery()}
        onChange={vi.fn()}
        guardrails={{
          min_improvement_ms: 5,
          min_improvement_ratio: null,
          max_transit_rtt_ms: 200,
          max_transit_stddev_ms: null,
        }}
      />,
    );
    expect(screen.getByTestId("filter-min-improvement-ms")).toHaveAttribute(
      "placeholder",
      "≥ 5 ms (guardrail)",
    );
    expect(screen.getByTestId("filter-max-transit-rtt-ms")).toHaveAttribute(
      "placeholder",
      "≤ 200 ms (guardrail)",
    );
    // Unset guardrails fall back to the generic placeholder.
    expect(screen.getByTestId("filter-min-improvement-ratio")).toHaveAttribute(
      "placeholder",
      "≥ … ratio",
    );
  });

  test("Qualifies-only switch toggles between true and null", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const { rerender } = render(
      <CandidatePairFilters value={makeQuery()} onChange={onChange} guardrails={NO_GUARDRAILS} />,
    );

    await user.click(screen.getByTestId("filter-qualifies-only"));
    expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ qualifies_only: true }));

    rerender(
      <CandidatePairFilters
        value={makeQuery({ qualifies_only: true })}
        onChange={onChange}
        guardrails={NO_GUARDRAILS}
      />,
    );
    await user.click(screen.getByTestId("filter-qualifies-only"));
    expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ qualifies_only: null }));
  });

  test("Reset filters clears every numeric input plus qualifies", async () => {
    const onChange = vi.fn();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(
      <CandidatePairFilters
        value={makeQuery({
          min_improvement_ms: 5,
          min_improvement_ratio: 0.1,
          max_transit_rtt_ms: 200,
          max_transit_stddev_ms: 15,
          qualifies_only: true,
        })}
        onChange={onChange}
        guardrails={NO_GUARDRAILS}
      />,
    );

    await user.click(screen.getByTestId("filter-reset"));
    expect(onChange).toHaveBeenLastCalledWith(
      expect.objectContaining({
        min_improvement_ms: null,
        min_improvement_ratio: null,
        max_transit_rtt_ms: null,
        max_transit_stddev_ms: null,
        qualifies_only: null,
      }),
    );
  });
});
