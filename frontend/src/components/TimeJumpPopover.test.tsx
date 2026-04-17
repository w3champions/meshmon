import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { TimeJumpPopover } from "./TimeJumpPopover";

const A_MS = Date.UTC(2026, 3, 17, 9, 12, 4);
const B_MS = Date.UTC(2026, 3, 17, 9, 14, 41);

function renderPopover(overrides: Partial<Parameters<typeof TimeJumpPopover>[0]> = {}) {
  const onRequestJump = vi.fn();
  const props = {
    anchorTimeMs: A_MS,
    otherMarkerMs: B_MS,
    side: "A" as const,
    onRequestJump,
    children: <button type="button">Jump to time…</button>,
    ...overrides,
  };
  render(<TimeJumpPopover {...props} />);
  return { onRequestJump };
}

describe("TimeJumpPopover", () => {
  it("fires onRequestJump with anchor − 5 min when the -5m quick-jump is clicked", async () => {
    const user = userEvent.setup();
    const { onRequestJump } = renderPopover();
    await user.click(screen.getByRole("button", { name: /jump to time/i }));
    await user.click(screen.getByRole("button", { name: /^-5m$/ }));
    expect(onRequestJump).toHaveBeenCalledWith(A_MS - 5 * 60 * 1_000);
  });

  it("disables a quick-jump whose target would cross the other marker", async () => {
    const user = userEvent.setup();
    renderPopover(); // side=A, anchor before B, so +1h crosses B
    await user.click(screen.getByRole("button", { name: /jump to time/i }));
    const plusHour = screen.getByRole("button", { name: /^\+1h$/ });
    expect(plusHour).toBeDisabled();
  });

  it("applies a datetime-local input value and fires onRequestJump", async () => {
    const user = userEvent.setup();
    const { onRequestJump } = renderPopover();
    await user.click(screen.getByRole("button", { name: /jump to time/i }));
    const input = screen.getByLabelText(/jump to/i);
    await user.clear(input);
    await user.type(input, "2026-04-17T09:10");
    await user.click(screen.getByRole("button", { name: /^go$/i }));
    expect(onRequestJump).toHaveBeenCalledWith(Date.UTC(2026, 3, 17, 9, 10, 0));
  });
});
