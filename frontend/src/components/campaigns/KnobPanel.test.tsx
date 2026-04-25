import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { KnobPanel } from "@/components/campaigns/KnobPanel";
import { type CampaignKnobs, DEFAULT_KNOBS } from "@/lib/campaign-config";

afterEach(() => {
  vi.clearAllMocks();
});

function baseKnobs(overrides: Partial<CampaignKnobs> = {}): CampaignKnobs {
  return { ...DEFAULT_KNOBS, ...overrides };
}

describe("KnobPanel", () => {
  test("renders the MTR hint only when protocol === 'mtr'", () => {
    const { rerender } = render(
      <KnobPanel value={baseKnobs({ protocol: "icmp" })} onChange={vi.fn()} />,
    );

    expect(screen.queryByText(/MTR is expensive/i)).not.toBeInTheDocument();

    rerender(<KnobPanel value={baseKnobs({ protocol: "mtr" })} onChange={vi.fn()} />);
    expect(screen.getByText(/MTR is expensive/i)).toBeInTheDocument();
  });

  test("toggling the force toggle emits force_measurement: true", async () => {
    const onChange = vi.fn<(next: CampaignKnobs) => void>();
    const user = userEvent.setup();

    render(<KnobPanel value={baseKnobs()} onChange={onChange} />);

    const toggle = screen.getByRole("button", { name: /force measurement/i });
    await user.click(toggle);

    expect(onChange).toHaveBeenCalled();
    const next = onChange.mock.calls.at(-1)?.[0];
    expect(next?.force_measurement).toBe(true);
  });

  test("switching evaluation_mode from optimization → diversity emits the change", async () => {
    const onChange = vi.fn<(next: CampaignKnobs) => void>();
    const user = userEvent.setup();

    render(<KnobPanel value={baseKnobs()} onChange={onChange} />);

    const diversityItem = screen.getByRole("radio", { name: /diversity/i });
    await user.click(diversityItem);

    const next = onChange.mock.calls.at(-1)?.[0];
    expect(next?.evaluation_mode).toBe("diversity");
  });

  test("clamps out-of-range numeric input on probe_count", () => {
    const onChange = vi.fn<(next: CampaignKnobs) => void>();

    render(<KnobPanel value={baseKnobs()} onChange={onChange} />);

    const input = screen.getByLabelText(/^probe count$/i) as HTMLInputElement;

    // Negative → clamps to min (1).
    fireEvent.change(input, { target: { value: "-3" } });
    const negCall = onChange.mock.calls.at(-1)?.[0];
    expect(negCall?.probe_count).toBe(1);

    // Above max (1000) → clamps to max.
    fireEvent.change(input, { target: { value: "999999" } });
    const bigCall = onChange.mock.calls.at(-1)?.[0];
    expect(bigCall?.probe_count).toBe(1000);
  });

  // ---------------------------------------------------------------------
  // Guardrail knobs — composer-time setup
  // ---------------------------------------------------------------------

  test("guardrail knobs default to empty inputs (null state)", () => {
    render(<KnobPanel value={baseKnobs()} onChange={vi.fn()} />);

    expect((screen.getByLabelText(/max transit rtt \(ms\)/i) as HTMLInputElement).value).toBe("");
    expect(
      (screen.getByLabelText(/max transit rtt stddev \(ms\)/i) as HTMLInputElement).value,
    ).toBe("");
    expect((screen.getByLabelText(/min improvement \(ms\)/i) as HTMLInputElement).value).toBe("");
    expect((screen.getByLabelText(/min improvement ratio/i) as HTMLInputElement).value).toBe("");
  });

  test("typing a value into max_transit_rtt_ms emits the parsed number", () => {
    const onChange = vi.fn<(next: CampaignKnobs) => void>();
    render(<KnobPanel value={baseKnobs()} onChange={onChange} />);

    fireEvent.change(screen.getByLabelText(/max transit rtt \(ms\)/i), {
      target: { value: "200" },
    });

    const next = onChange.mock.calls.at(-1)?.[0];
    expect(next?.max_transit_rtt_ms).toBe(200);
  });

  test("clearing a guardrail input emits null", () => {
    const onChange = vi.fn<(next: CampaignKnobs) => void>();
    render(<KnobPanel value={baseKnobs({ max_transit_rtt_ms: 250 })} onChange={onChange} />);

    const input = screen.getByLabelText(/max transit rtt \(ms\)/i) as HTMLInputElement;
    expect(input.value).toBe("250");

    fireEvent.change(input, { target: { value: "" } });

    const next = onChange.mock.calls.at(-1)?.[0];
    expect(next?.max_transit_rtt_ms).toBeNull();
  });

  test("min_improvement_ms accepts negative values", () => {
    const onChange = vi.fn<(next: CampaignKnobs) => void>();
    render(<KnobPanel value={baseKnobs()} onChange={onChange} />);

    fireEvent.change(screen.getByLabelText(/min improvement \(ms\)/i), {
      target: { value: "-25" },
    });

    const next = onChange.mock.calls.at(-1)?.[0];
    expect(next?.min_improvement_ms).toBe(-25);
  });

  test("clamps out-of-range guardrail input", () => {
    const onChange = vi.fn<(next: CampaignKnobs) => void>();
    render(<KnobPanel value={baseKnobs()} onChange={onChange} />);

    const input = screen.getByLabelText(/max transit rtt \(ms\)/i) as HTMLInputElement;

    // Above max (10000) → clamps to max.
    fireEvent.change(input, { target: { value: "999999" } });
    const big = onChange.mock.calls.at(-1)?.[0];
    expect(big?.max_transit_rtt_ms).toBe(10000);

    // Below min (1) → clamps to min.
    fireEvent.change(input, { target: { value: "-50" } });
    const neg = onChange.mock.calls.at(-1)?.[0];
    expect(neg?.max_transit_rtt_ms).toBe(1);
  });
});
