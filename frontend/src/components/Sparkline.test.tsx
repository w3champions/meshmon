import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { Sparkline } from "@/components/Sparkline";

describe("Sparkline", () => {
  test("renders a labelled chart when samples are provided", () => {
    render(
      <Sparkline
        samples={[
          [0, 1],
          [60_000, 2],
          [120_000, 1.5],
        ]}
        ariaLabel="RTT"
      />,
    );
    expect(screen.getByLabelText("RTT")).toBeInTheDocument();
  });

  test("renders an 'n/a' placeholder when empty", () => {
    render(<Sparkline samples={[]} ariaLabel="Loss" />);
    const el = screen.getByLabelText("Loss");
    expect(el).toHaveTextContent(/n\/a/i);
  });
});
