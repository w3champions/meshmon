import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { components } from "@/api/schema.gen";
import { RouteHistoryTable } from "@/components/RouteHistoryTable";

type Row = components["schemas"]["RouteSnapshotSummary"];

const rows: Row[] = [
  {
    id: 2,
    source_id: "a",
    target_id: "b",
    protocol: "icmp",
    observed_at: "2026-04-13T10:10:00Z",
    path_summary: { avg_rtt_micros: 185_000, loss_pct: 0, hop_count: 5 },
  },
  {
    id: 1,
    source_id: "a",
    target_id: "b",
    protocol: "icmp",
    observed_at: "2026-04-13T09:30:00Z",
    path_summary: { avg_rtt_micros: 290_000, loss_pct: 0.038, hop_count: 6 },
  },
];

afterEach(cleanup);

describe("RouteHistoryTable", () => {
  test("renders one row per snapshot in descending order", () => {
    render(<RouteHistoryTable snapshots={rows} onCompare={() => {}} />);
    const bodyRows = screen.getAllByRole("row").slice(1);
    expect(bodyRows).toHaveLength(2);
    expect(bodyRows[0]).toHaveTextContent(/5 hops/i);
    expect(bodyRows[1]).toHaveTextContent(/6 hops/i);
  });

  test("picking A then B enables the Compare button and fires onCompare", async () => {
    const onCompare = vi.fn();
    render(<RouteHistoryTable snapshots={rows} onCompare={onCompare} />);
    const user = userEvent.setup();
    await user.click(screen.getAllByRole("radio", { name: /pick as a/i })[0]);
    await user.click(screen.getAllByRole("radio", { name: /pick as b/i })[1]);
    const btn = screen.getByRole("button", { name: /compare/i });
    expect(btn).toBeEnabled();
    await user.click(btn);
    expect(onCompare).toHaveBeenCalledWith({ a: 2, b: 1 });
  });

  test("empty snapshots renders a placeholder", () => {
    render(<RouteHistoryTable snapshots={[]} onCompare={() => {}} />);
    expect(screen.getByText(/no route snapshots/i)).toBeInTheDocument();
  });

  test("renders a truncation footnote when `truncated` is true", () => {
    render(<RouteHistoryTable snapshots={rows} truncated onCompare={() => {}} />);
    expect(screen.getByText(/showing latest 100/i)).toBeInTheDocument();
  });

  test("omits the truncation footnote when `truncated` is false or missing", () => {
    render(<RouteHistoryTable snapshots={rows} onCompare={() => {}} />);
    expect(screen.queryByText(/showing latest 100/i)).toBeNull();
  });
});
