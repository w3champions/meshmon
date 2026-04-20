import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { ReenrichConfirm } from "@/components/catalogue/ReenrichConfirm";

describe("ReenrichConfirm", () => {
  test("renders title and body with the formatted selection size", () => {
    render(<ReenrichConfirm selectionSize={42} open onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByText("Re-enrich 42 rows?")).toBeInTheDocument();
    expect(screen.getByText("This will consume ~42 ipgeolocation credits.")).toBeInTheDocument();
  });

  test("OK button fires onConfirm", async () => {
    const onConfirm = vi.fn();
    render(<ReenrichConfirm selectionSize={25} open onConfirm={onConfirm} onCancel={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Re-enrich" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  test("Cancel button fires onCancel", async () => {
    const onCancel = vi.fn();
    render(<ReenrichConfirm selectionSize={25} open onConfirm={vi.fn()} onCancel={onCancel} />);
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  test("is not in the DOM when open is false", () => {
    render(
      <ReenrichConfirm selectionSize={30} open={false} onConfirm={vi.fn()} onCancel={vi.fn()} />,
    );
    expect(screen.queryByText(/Re-enrich 30 rows\?/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Re-enrich" })).not.toBeInTheDocument();
  });
});
