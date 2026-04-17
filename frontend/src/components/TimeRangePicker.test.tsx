import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
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
});
