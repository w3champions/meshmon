import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import "@/test/cytoscape-mock";
import type { HistoryMeasurement } from "@/api/hooks/history";
import type { components } from "@/api/schema.gen";
import { IpHostnameProvider } from "@/components/ip-hostname/IpHostnameProvider";
import { instances } from "@/test/cytoscape-mock";
import { MtrTracesList } from "./MtrTracesList";

type HopJson = components["schemas"]["HopJson"];

function hop(over: Partial<HopJson>): HopJson {
  return {
    position: 1,
    observed_ips: [{ ip: "10.0.0.1", freq: 1 }],
    avg_rtt_micros: 1_000,
    stddev_rtt_micros: 100,
    loss_pct: 0,
    ...over,
  };
}

function measurement(over: Partial<HistoryMeasurement>): HistoryMeasurement {
  return {
    id: 1,
    source_agent_id: "src-a",
    destination_ip: "10.0.0.1",
    protocol: "icmp",
    kind: "detail_mtr",
    measured_at: "2026-04-20T00:00:00.000Z",
    probe_count: 10,
    loss_pct: 0,
    latency_avg_ms: 1.2,
    latency_min_ms: 1.0,
    latency_max_ms: 1.5,
    latency_p95_ms: 1.4,
    latency_stddev_ms: 0.1,
    mtr_captured_at: "2026-04-20T00:00:00.000Z",
    mtr_hops: [
      hop({ position: 1 }),
      hop({ position: 2, observed_ips: [{ ip: "10.0.0.2", freq: 1 }] }),
    ],
    ...over,
  };
}

describe("MtrTracesList", () => {
  test("renders a status line when no measurements carry MTR hops", () => {
    instances.length = 0;
    render(
      <IpHostnameProvider>
        <MtrTracesList
          measurements={[
            measurement({ id: 1, kind: "detail_ping", mtr_captured_at: null, mtr_hops: null }),
          ]}
        />
      </IpHostnameProvider>,
    );
    expect(screen.getByRole("status")).toHaveTextContent(/no mtr traces/i);
  });

  test("renders newest-first and mounts RouteTopology only when expanded", () => {
    instances.length = 0;
    render(
      <IpHostnameProvider>
        <MtrTracesList
          measurements={[
            measurement({
              id: 1,
              protocol: "icmp",
              mtr_captured_at: "2026-04-20T00:00:00.000Z",
            }),
            measurement({
              id: 2,
              protocol: "tcp",
              mtr_captured_at: "2026-04-20T02:00:00.000Z",
            }),
            measurement({
              id: 3,
              protocol: "udp",
              mtr_captured_at: "2026-04-20T01:00:00.000Z",
            }),
          ]}
        />
      </IpHostnameProvider>,
    );

    const list = screen.getByRole("list", { name: /mtr traces/i });
    const items = list.querySelectorAll("li");
    expect(items).toHaveLength(3);
    // Newest first: TCP (02:00) → UDP (01:00) → ICMP (00:00).
    expect(items[0]?.textContent?.toLowerCase()).toContain("tcp");
    expect(items[1]?.textContent?.toLowerCase()).toContain("udp");
    expect(items[2]?.textContent?.toLowerCase()).toContain("icmp");

    // RouteTopology only mounts when a row is expanded. Collapsed by default.
    expect(instances).toHaveLength(0);

    const firstToggle = items[0]?.querySelector("button");
    expect(firstToggle).not.toBeNull();
    fireEvent.click(firstToggle as HTMLElement);

    expect(instances).toHaveLength(1);
    // Hops reach RouteTopology verbatim — each hop's dominant IP surfaces
    // in the cytoscape node label, so asserting on the label strings is
    // the simplest pass-through check.
    const labels = instances[0].elements
      .map((el) => (el as { data: { label?: string } }).data.label ?? "")
      .filter((l) => l.length > 0);
    expect(labels.some((l) => l.includes("10.0.0.1"))).toBe(true);
    expect(labels.some((l) => l.includes("10.0.0.2"))).toBe(true);
  });

  test("falls back to measured_at when mtr_captured_at is null", () => {
    instances.length = 0;
    render(
      <IpHostnameProvider>
        <MtrTracesList
          measurements={[
            measurement({
              id: 1,
              measured_at: "2026-04-20T00:00:00.000Z",
              mtr_captured_at: null,
            }),
          ]}
        />
      </IpHostnameProvider>,
    );
    expect(screen.getByRole("list", { name: /mtr traces/i })).toBeInTheDocument();
  });
});
