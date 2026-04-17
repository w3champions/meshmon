import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import type { components } from "@/api/schema.gen";
import { HopDetailCard } from "@/components/HopDetailCard";

type HopJson = components["schemas"]["HopJson"];

const HOP: HopJson = {
  position: 4,
  observed_ips: [
    { ip: "10.0.0.5", freq: 8 },
    { ip: "10.0.0.6", freq: 2 },
  ],
  avg_rtt_micros: 12_345,
  stddev_rtt_micros: 678,
  loss_pct: 0.017,
};

describe("HopDetailCard", () => {
  test("renders hop metadata", () => {
    render(<HopDetailCard hop={HOP} onClose={() => {}} />);
    expect(screen.getByText(/hop 4/i)).toBeInTheDocument();
    expect(screen.getByText("10.0.0.5")).toBeInTheDocument();
    expect(screen.getByText(/×8/)).toBeInTheDocument();
    expect(screen.getByText("10.0.0.6")).toBeInTheDocument();
    expect(screen.getByText(/12\.35 ms/)).toBeInTheDocument();
    expect(screen.getByText(/0\.68 ms/)).toBeInTheDocument();
    expect(screen.getByText(/1\.7%/)).toBeInTheDocument();
  });

  test("close button fires onClose", async () => {
    const onClose = vi.fn();
    render(<HopDetailCard hop={HOP} onClose={onClose} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /close hop detail/i }));
    expect(onClose).toHaveBeenCalledOnce();
  });
});
