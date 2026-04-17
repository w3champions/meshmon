import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { CustomRangeInputs } from "./CustomRangeInputs";

describe("CustomRangeInputs", () => {
  it("renders paired datetime-local inputs", () => {
    render(<CustomRangeInputs from="" to="" onChange={() => {}} />);
    expect(screen.getByLabelText(/from/i)).toHaveAttribute(
      "type",
      "datetime-local",
    );
    expect(screen.getByLabelText(/to/i)).toHaveAttribute(
      "type",
      "datetime-local",
    );
  });

  it("emits ISO-8601 strings on change", () => {
    const onChange = vi.fn();
    render(<CustomRangeInputs from="" to="" onChange={onChange} />);
    fireEvent.change(screen.getByLabelText(/from/i), {
      target: { value: "2026-04-13T10:15" },
    });
    expect(onChange).toHaveBeenCalledTimes(1);
    const [[args]] = onChange.mock.calls;
    expect(args.to).toBe("");
    // Local-time interpretation varies by runner timezone; just ensure
    // ISO-8601 with the same date survives the round-trip.
    expect(args.from).toMatch(/^2026-04-13T/);
    expect(args.from).toMatch(/Z$/);
  });

  it("is controlled — current ISO values populate the inputs", () => {
    render(
      <CustomRangeInputs
        from="2026-04-13T10:15:00Z"
        to="2026-04-13T14:30:00Z"
        onChange={() => {}}
      />,
    );
    // datetime-local uses local time; the rendered value should at least
    // carry the date.
    expect(
      (screen.getByLabelText(/from/i) as HTMLInputElement).value,
    ).toMatch(/^2026-04-1[23]T/);
  });

  it("empty datetime-local clears the ISO string", () => {
    const onChange = vi.fn();
    render(
      <CustomRangeInputs
        from="2026-04-13T10:15:00Z"
        to=""
        onChange={onChange}
      />,
    );
    fireEvent.change(screen.getByLabelText(/from/i), { target: { value: "" } });
    expect(onChange).toHaveBeenCalledWith({ from: "", to: "" });
  });
});
