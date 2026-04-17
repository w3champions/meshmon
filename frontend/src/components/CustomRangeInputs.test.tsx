import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { CustomRangeInputs } from "./CustomRangeInputs";

describe("CustomRangeInputs", () => {
  it("renders paired datetime-local inputs", () => {
    render(<CustomRangeInputs from="" to="" onChange={() => {}} />);
    expect(screen.getByLabelText(/from/i)).toHaveAttribute("type", "datetime-local");
    expect(screen.getByLabelText(/to/i)).toHaveAttribute("type", "datetime-local");
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
    expect((screen.getByLabelText(/from/i) as HTMLInputElement).value).toMatch(/^2026-04-1[23]T/);
  });

  it("empty datetime-local clears the ISO string", () => {
    const onChange = vi.fn();
    render(<CustomRangeInputs from="2026-04-13T10:15:00Z" to="" onChange={onChange} />);
    fireEvent.change(screen.getByLabelText(/from/i), { target: { value: "" } });
    expect(onChange).toHaveBeenCalledWith({ from: "", to: "" });
  });

  it("generates unique ids so two instances can coexist on one page", () => {
    // Hardcoded ids would collide in the DOM and break each other's
    // label-to-input association. Render two pickers and confirm ids are
    // unique and each label still targets the correct input.
    render(
      <>
        <CustomRangeInputs from="" to="" onChange={() => {}} />
        <CustomRangeInputs from="" to="" onChange={() => {}} />
      </>,
    );
    const froms = screen.getAllByLabelText(/from/i);
    const tos = screen.getAllByLabelText(/to/i);
    expect(froms).toHaveLength(2);
    expect(tos).toHaveLength(2);
    // Each input must carry a non-empty id, and those ids must differ
    // across the two instances.
    const fromIds = froms.map((el) => el.id);
    const toIds = tos.map((el) => el.id);
    expect(fromIds[0]).toBeTruthy();
    expect(fromIds[1]).toBeTruthy();
    expect(fromIds[0]).not.toBe(fromIds[1]);
    expect(toIds[0]).not.toBe(toIds[1]);
  });
});
