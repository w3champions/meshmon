import { screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import type { AgentSummary } from "@/api/hooks/agents";
import type { HealthMatrix } from "@/api/hooks/health-matrix";
import { PathHealthGrid } from "@/components/PathHealthGrid";
import { renderWithProviders } from "@/test/query-wrapper";

// Minimal AgentSummary fixture — only `id` is used by PathHealthGrid.
function agent(id: string): AgentSummary {
  return { id } as AgentSummary;
}

const AGENTS: AgentSummary[] = [agent("alpha"), agent("beta"), agent("gamma")];

function matrix(pairs: Array<[string, string, number]>): HealthMatrix {
  const m: HealthMatrix = new Map();
  for (const [src, tgt, rate] of pairs) {
    const state = rate >= 0.2 ? "unreachable" : rate >= 0.05 ? "degraded" : "normal";
    m.set(`${src}>${tgt}`, { source: src, target: tgt, failureRate: rate, state });
  }
  return m;
}

describe("PathHealthGrid", () => {
  // Test 1: renders one cell per (source, target) pair including self-cells.
  test("renders one cell per (source, target) pair for provided agents", async () => {
    renderWithProviders(<PathHealthGrid agents={AGENTS} matrix={new Map()} />);

    // 3 agents → 3×3 = 9 cells total (including self-cells).
    // Each cell is a <Link> with aria-label "X to Y, stale".
    const links = await screen.findAllByRole("link");
    expect(links).toHaveLength(9);

    // Row and column headers for each agent ID.
    const rowHeaders = screen.getAllByTestId("row-header");
    expect(rowHeaders).toHaveLength(3);
    const colHeaders = screen.getAllByTestId("col-header");
    expect(colHeaders).toHaveLength(3);
  });

  // Test 2: cell data-state attribute reflects the HealthState from the matrix.
  test("cell data-state reflects HealthState from the matrix prop", async () => {
    const m = matrix([
      ["alpha", "beta", 0.01], // normal
      ["alpha", "gamma", 0.1], // degraded
      ["beta", "alpha", 0.3], // unreachable
    ]);

    renderWithProviders(<PathHealthGrid agents={AGENTS} matrix={m} />);

    // Wait for the grid to render.
    await screen.findAllByRole("link");

    const normalLink = screen.getByRole("link", {
      name: "alpha to beta, normal",
    });
    expect(normalLink).toHaveAttribute("data-state", "normal");

    const degradedLink = screen.getByRole("link", {
      name: "alpha to gamma, degraded",
    });
    expect(degradedLink).toHaveAttribute("data-state", "degraded");

    const unreachableLink = screen.getByRole("link", {
      name: "beta to alpha, unreachable",
    });
    expect(unreachableLink).toHaveAttribute("data-state", "unreachable");
  });

  // Test 3: clicking a cell navigates to /paths/$source/$target (Link href).
  test("each cell is a link with the correct /paths/$source/$target href", async () => {
    renderWithProviders(<PathHealthGrid agents={[agent("a"), agent("b")]} matrix={new Map()} />);

    // 2 agents → 4 links
    const link = await screen.findByRole("link", { name: "a to b, stale" });
    expect(link).toHaveAttribute("href", "/paths/a/b");

    const selfLink = screen.getByRole("link", { name: "a to a, stale" });
    expect(selfLink).toHaveAttribute("href", "/paths/a/a");
  });

  // Test 4: missing matrix entry → data-state="stale".
  test("missing matrix entry gives cell data-state='stale'", async () => {
    // alpha>beta is not in the matrix.
    const m = matrix([["alpha", "gamma", 0.0]]);

    renderWithProviders(<PathHealthGrid agents={AGENTS} matrix={m} />);

    await screen.findAllByRole("link");

    const staleLink = screen.getByRole("link", {
      name: "alpha to beta, stale",
    });
    expect(staleLink).toHaveAttribute("data-state", "stale");
  });

  // Test 5a: sourceFilter renders only one row.
  test("sourceFilter renders only matching source rows", async () => {
    renderWithProviders(<PathHealthGrid agents={AGENTS} matrix={new Map()} sourceFilter="alpha" />);

    await screen.findAllByRole("link");

    // Only 1 row header, 3 column headers.
    const rowHeaders = screen.getAllByTestId("row-header");
    expect(rowHeaders).toHaveLength(1);
    expect(rowHeaders[0].textContent).toBe("alpha");

    // 1 row × 3 cols = 3 cells.
    const links = screen.getAllByRole("link");
    expect(links).toHaveLength(3);
  });

  // Test 5b: targetFilter renders only one column.
  test("targetFilter renders only matching target columns", async () => {
    renderWithProviders(<PathHealthGrid agents={AGENTS} matrix={new Map()} targetFilter="beta" />);

    await screen.findAllByRole("link");

    // 3 row headers, 1 column header.
    const rowHeaders = screen.getAllByTestId("row-header");
    expect(rowHeaders).toHaveLength(3);
    const colHeaders = screen.getAllByTestId("col-header");
    expect(colHeaders).toHaveLength(1);
    expect(colHeaders[0].textContent).toBe("beta");

    // 3 rows × 1 col = 3 cells.
    const links = screen.getAllByRole("link");
    expect(links).toHaveLength(3);
  });

  // Test 6: empty agents array → fallback text.
  test("shows fallback text for empty agents array", async () => {
    renderWithProviders(<PathHealthGrid agents={[]} matrix={new Map()} />);

    expect(await screen.findByText("No agents registered yet.")).toBeInTheDocument();
    expect(screen.queryByRole("link")).not.toBeInTheDocument();
  });
});
