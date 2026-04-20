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
});
