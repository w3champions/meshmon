import { describe, expect, it } from "vitest";
import type { AlertSummary } from "@/api/hooks/alerts";
import {
  defaultAlertFilter,
  filterAlerts,
  uniqueCategories,
} from "./alerts-filter";

function makeAlert(overrides: Partial<AlertSummary>): AlertSummary {
  return {
    fingerprint: "abc",
    starts_at: "2026-04-13T10:00:00Z",
    ends_at: "0001-01-01T00:00:00Z",
    state: "active",
    labels: {},
    summary: null,
    description: null,
    ...overrides,
  } as AlertSummary;
}

describe("filterAlerts", () => {
  const alerts: AlertSummary[] = [
    makeAlert({
      fingerprint: "a",
      labels: {
        alertname: "PathPacketLoss",
        severity: "critical",
        category: "loss",
        source: "brazil-north",
        target: "paris-core",
      },
      summary: "packet loss on br→paris",
    }),
    makeAlert({
      fingerprint: "b",
      labels: {
        alertname: "PathLatencyRegression",
        severity: "warning",
        category: "latency",
        source: "tokyo-edge",
        target: "paris-core",
      },
      summary: "latency regression",
    }),
    makeAlert({
      fingerprint: "c",
      labels: { alertname: "AgentOffline", severity: "warning", category: "agent" },
      summary: "agent offline",
    }),
  ];

  it("no-op when all filters are at defaults", () => {
    expect(
      filterAlerts(alerts, defaultAlertFilter()).map((a) => a.fingerprint),
    ).toEqual(["a", "b", "c"]);
  });

  it("severity filter keeps matching rows", () => {
    const f = { ...defaultAlertFilter(), severity: "critical" as const };
    expect(filterAlerts(alerts, f).map((a) => a.fingerprint)).toEqual(["a"]);
  });

  it("category filter respects 'all'", () => {
    const f = { ...defaultAlertFilter(), category: "loss" };
    expect(filterAlerts(alerts, f).map((a) => a.fingerprint)).toEqual(["a"]);
  });

  it("source is a case-insensitive substring match on labels.source", () => {
    const f = { ...defaultAlertFilter(), source: "BRAZIL" };
    expect(filterAlerts(alerts, f).map((a) => a.fingerprint)).toEqual(["a"]);
  });

  it("target is a case-insensitive substring match on labels.target", () => {
    const f = { ...defaultAlertFilter(), target: "paris" };
    expect(filterAlerts(alerts, f).map((a) => a.fingerprint)).toEqual([
      "a",
      "b",
    ]);
  });

  it("text searches alertname, summary, description case-insensitively", () => {
    const f = { ...defaultAlertFilter(), text: "regression" };
    expect(filterAlerts(alerts, f).map((a) => a.fingerprint)).toEqual(["b"]);

    const f2 = { ...defaultAlertFilter(), text: "offline" };
    expect(filterAlerts(alerts, f2).map((a) => a.fingerprint)).toEqual(["c"]);
  });

  it("combines filters with AND semantics", () => {
    const f = {
      ...defaultAlertFilter(),
      severity: "warning" as const,
      target: "paris",
    };
    expect(filterAlerts(alerts, f).map((a) => a.fingerprint)).toEqual(["b"]);
  });
});

describe("uniqueCategories", () => {
  it("returns the sorted set of categories, excluding empties", () => {
    const alerts: AlertSummary[] = [
      makeAlert({ labels: { category: "loss" } }),
      makeAlert({ labels: { category: "latency" } }),
      makeAlert({ labels: { category: "loss" } }),
      makeAlert({ labels: {} }),
    ];
    expect(uniqueCategories(alerts)).toEqual(["latency", "loss"]);
  });
});
