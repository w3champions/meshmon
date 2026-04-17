import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { TimeRangePicker } from "@/components/TimeRangePicker";

describe("TimeRangePicker", () => {
  test("renders the current value as the selected option", () => {
    render(<TimeRangePicker value="7d" onChange={() => {}} />);
    expect(screen.getByRole("combobox")).toHaveTextContent("Last 7 days");
  });

  test("calls onChange with the next preset", async () => {
    const handler = vi.fn();
    render(<TimeRangePicker value="24h" onChange={handler} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox"));
    await user.click(await screen.findByRole("option", { name: /last 1 hour/i }));
    expect(handler).toHaveBeenCalledWith({ range: "1h" });
  });

  test("exposes 'Custom' as a selectable option", async () => {
    render(<TimeRangePicker value="24h" onChange={() => {}} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox"));
    expect(await screen.findByRole("option", { name: /custom/i })).toBeInTheDocument();
  });

  test("renders CustomRangeInputs when value is 'custom'", () => {
    render(
      <TimeRangePicker
        value="custom"
        from="2026-04-13T10:15:00Z"
        to="2026-04-13T14:30:00Z"
        onChange={() => {}}
      />,
    );
    expect(screen.getByLabelText(/from/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/to/i)).toBeInTheDocument();
  });

  test("forwards from/to on custom-range change", async () => {
    const handler = vi.fn();
    const { rerender } = render(
      <TimeRangePicker
        value="24h"
        from="2026-04-13T10:15:00Z"
        to="2026-04-13T14:30:00Z"
        onChange={handler}
      />,
    );
    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox"));
    await user.click(await screen.findByRole("option", { name: /custom/i }));
    // Selecting 'custom' forwards the current from/to so the preset switch
    // is non-lossy and the inputs render populated.
    expect(handler).toHaveBeenCalledWith({
      range: "custom",
      from: "2026-04-13T10:15:00Z",
      to: "2026-04-13T14:30:00Z",
    });

    rerender(
      <TimeRangePicker
        value="custom"
        from="2026-04-13T10:15:00Z"
        to="2026-04-13T14:30:00Z"
        onChange={handler}
      />,
    );
    handler.mockClear();
    // Editing the 'from' input emits an updated ISO string + original 'to'.
    fireEvent.change(screen.getByLabelText(/from/i), {
      target: { value: "2026-04-13T09:00" },
    });
    expect(handler).toHaveBeenCalledTimes(1);
    const [[emitted]] = handler.mock.calls;
    expect(emitted.range).toBe("custom");
    expect(emitted.to).toBe("2026-04-13T14:30:00Z");
    expect(emitted.from).toMatch(/^2026-04-13T/);
  });

  test("seeds ISO from/to when switching to 'custom' from a preset without bounds", async () => {
    const handler = vi.fn();
    // Starting on a preset with no custom bounds supplied — the common
    // default-route scenario. Without seeding, the router schema rejects
    // `custom` with empty from/to and the preset switch silently fails.
    render(<TimeRangePicker value="24h" onChange={handler} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox"));
    await user.click(await screen.findByRole("option", { name: /custom/i }));

    expect(handler).toHaveBeenCalledTimes(1);
    const [[emitted]] = handler.mock.calls;
    expect(emitted.range).toBe("custom");
    // Must be non-empty ISO-8601 strings so the router schema accepts them.
    expect(typeof emitted.from).toBe("string");
    expect(typeof emitted.to).toBe("string");
    expect(emitted.from).not.toBe("");
    expect(emitted.to).not.toBe("");
    // ISO-8601 with trailing 'Z' (UTC); Date.parse must succeed.
    expect(emitted.from).toMatch(/^\d{4}-\d{2}-\d{2}T.*Z$/);
    expect(emitted.to).toMatch(/^\d{4}-\d{2}-\d{2}T.*Z$/);
    expect(Number.isNaN(Date.parse(emitted.from))).toBe(false);
    expect(Number.isNaN(Date.parse(emitted.to))).toBe(false);
  });
});
