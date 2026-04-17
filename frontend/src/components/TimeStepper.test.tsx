import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { TimeStepper } from "./TimeStepper";

function sum(id: number, observed_at: string) {
  return { id, source_id: "a", target_id: "b", protocol: "icmp", observed_at };
}

describe("TimeStepper", () => {
  it("shows time deltas to prev and next on the arrow faces", () => {
    const selectedMs = Date.UTC(2026, 3, 17, 9, 12, 4);
    render(
      <TimeStepper
        side="A"
        selectedMs={selectedMs}
        prev={sum(3, "2026-04-17T09:09:10Z")}
        next={sum(5, "2026-04-17T09:13:08Z")}
        onStep={() => {}}
      />,
    );
    expect(screen.getByRole("button", { name: /2m 54s earlier/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /1m 4s later/i })).toBeInTheDocument();
  });

  it("disables the prev button when prev is undefined", () => {
    const selectedMs = Date.UTC(2026, 3, 17, 9, 12, 4);
    render(
      <TimeStepper
        side="A"
        selectedMs={selectedMs}
        next={sum(5, "2026-04-17T09:13:08Z")}
        onStep={() => {}}
      />,
    );
    const prevBtn = screen.getByRole("button", { name: /no earlier/i });
    expect(prevBtn).toBeDisabled();
  });

  it("fires onStep with the prev snapshot when the prev button is clicked", async () => {
    const user = userEvent.setup();
    const onStep = vi.fn();
    const selectedMs = Date.UTC(2026, 3, 17, 9, 12, 4);
    const prev = sum(3, "2026-04-17T09:09:10Z");
    render(
      <TimeStepper
        side="A"
        selectedMs={selectedMs}
        prev={prev}
        next={sum(5, "2026-04-17T09:13:08Z")}
        onStep={onStep}
      />,
    );
    await user.click(screen.getByRole("button", { name: /earlier/i }));
    expect(onStep).toHaveBeenCalledWith(prev);
  });
});
