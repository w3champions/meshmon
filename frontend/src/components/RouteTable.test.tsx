import "@testing-library/jest-dom/vitest";
import { render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { components } from "@/api/schema.gen";
import { IpHostnameProvider } from "@/components/ip-hostname";
import { RouteTable } from "./RouteTable";

type HopJson = components["schemas"]["HopJson"];

class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

beforeEach(() => {
  vi.stubGlobal("EventSource", NoopEventSource);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function hop(position: number, ip: string, rtt_us: number, loss_ratio: number, freq = 1): HopJson {
  return {
    position,
    avg_rtt_micros: rtt_us,
    loss_ratio,
    observed_ips: [{ ip, freq }],
    stddev_rtt_micros: 0,
  };
}

function renderWithProvider(ui: React.ReactElement) {
  return render(<IpHostnameProvider>{ui}</IpHostnameProvider>);
}

describe("RouteTable", () => {
  it("renders one row per hop with formatted values", () => {
    renderWithProvider(
      <RouteTable hops={[hop(1, "10.0.0.1", 1_200, 0), hop(2, "10.0.0.2", 50_000, 0.02, 0.75)]} />,
    );
    const rows = screen.getAllByRole("row");
    expect(rows).toHaveLength(3); // 1 header + 2 body

    const r1 = within(rows[1]);
    expect(r1.getByText("1")).toBeInTheDocument();
    // IpHostname renders the bare IP on a cold miss — no provider-seeded
    // hostname in this test, so the cell text is the IP itself.
    expect(r1.getByText("10.0.0.1")).toBeInTheDocument();
    expect(r1.getByText(/1\.2\s?ms/)).toBeInTheDocument();
    expect(r1.getByText("0.00%")).toBeInTheDocument();

    const r2 = within(rows[2]);
    expect(r2.getByText("75%")).toBeInTheDocument();
    expect(r2.getByText("2.00%")).toBeInTheDocument();
  });

  it("renders the Hostname column header", () => {
    renderWithProvider(<RouteTable hops={[hop(1, "10.0.0.1", 1_000, 0)]} />);
    expect(screen.getByRole("columnheader", { name: /hostname/i })).toBeInTheDocument();
  });

  it("renders bare IP fallback when the provider map has no entry for the hop IP", () => {
    const hopWithHostname: HopJson = {
      position: 1,
      avg_rtt_micros: 1_000,
      loss_ratio: 0,
      observed_ips: [{ ip: "10.0.0.1", hostname: "router.example.com", freq: 1 }],
      stddev_rtt_micros: 0,
    };
    // IpHostname resolves through the provider map, not from the DTO
    // `hostname` field directly. The map is populated by
    // useSeedHostnamesOnResponse at query-hook level (covered by the hook
    // tests); rendering RouteTable alone leaves the map empty, so the cell
    // falls back to the bare IP. Full seeded-hostname rendering is covered
    // by IpHostname's own unit tests.
    renderWithProvider(<RouteTable hops={[hopWithHostname]} />);
    expect(screen.getByText("10.0.0.1")).toBeInTheDocument();
  });

  it("renders an empty-state row when hops is empty", () => {
    renderWithProvider(<RouteTable hops={[]} />);
    expect(screen.getByText(/no hops recorded/i)).toBeInTheDocument();
  });

  it("highlights changed rows when a diff is provided", () => {
    const hops = [hop(1, "10.0.0.1", 1_000, 0), hop(2, "10.0.0.9", 2_000, 0)];
    renderWithProvider(
      <RouteTable
        hops={hops}
        diff={{
          changedPositions: new Set([2]),
          addedPositions: new Set<number>(),
          removedPositions: new Set<number>(),
        }}
      />,
    );
    const rows = screen.getAllByRole("row");
    expect(rows[2]).toHaveAttribute("data-diff-state", "changed");
    expect(within(rows[2]).getByText(/★ changed/i)).toBeInTheDocument();
  });

  it("marks added rows with data-diff-state=added", () => {
    const hops = [hop(2, "10.0.0.5", 1_000, 0)];
    renderWithProvider(
      <RouteTable
        hops={hops}
        diff={{
          changedPositions: new Set<number>(),
          addedPositions: new Set([2]),
          removedPositions: new Set<number>(),
        }}
      />,
    );
    const rows = screen.getAllByRole("row");
    expect(rows[1]).toHaveAttribute("data-diff-state", "added");
    expect(within(rows[1]).getByText(/\+ added/i)).toBeInTheDocument();
  });
});
