import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { StatusChip } from "@/components/catalogue/StatusChip";

describe("StatusChip", () => {
  test("renders 'Enriched' with emerald classes for enriched status", () => {
    render(<StatusChip status="enriched" />);
    const chip = screen.getByText("Enriched");
    expect(chip.className).toMatch(/emerald/);
  });

  test("renders 'Pending' and does NOT fire onReenrich when clicked", async () => {
    const user = userEvent.setup();
    const onReenrich = vi.fn();
    render(<StatusChip status="pending" onReenrich={onReenrich} />);
    const chip = screen.getByText("Pending");
    await user.click(chip);
    expect(onReenrich).not.toHaveBeenCalled();
  });

  test("renders 'Failed' and fires onReenrich on click", async () => {
    const user = userEvent.setup();
    const onReenrich = vi.fn();
    render(<StatusChip status="failed" onReenrich={onReenrich} />);
    await user.click(screen.getByRole("button", { name: /Re-enrich \(Failed\)/ }));
    expect(onReenrich).toHaveBeenCalledTimes(1);
  });

  test("Enter key fires onReenrich when status is actionable", async () => {
    const user = userEvent.setup();
    const onReenrich = vi.fn();
    render(<StatusChip status="enriched" onReenrich={onReenrich} />);
    const chip = screen.getByRole("button", { name: /Re-enrich \(Enriched\)/ });
    chip.focus();
    await user.keyboard("{Enter}");
    expect(onReenrich).toHaveBeenCalledTimes(1);
  });

  test("operatorLocked toggles the lock badge", () => {
    const { rerender } = render(<StatusChip status="enriched" operatorLocked={true} />);
    expect(screen.getByLabelText("Operator-edited")).toBeInTheDocument();

    rerender(<StatusChip status="enriched" operatorLocked={false} />);
    expect(screen.queryByLabelText("Operator-edited")).not.toBeInTheDocument();

    rerender(<StatusChip status="enriched" />);
    expect(screen.queryByLabelText("Operator-edited")).not.toBeInTheDocument();
  });
});
