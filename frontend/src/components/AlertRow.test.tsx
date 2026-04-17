import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { AlertSummary } from "@/api/hooks/alerts";
import { AlertRow } from "./AlertRow";

function mkAlert(overrides: Partial<AlertSummary> = {}): AlertSummary {
  return {
    fingerprint: "abcd",
    starts_at: new Date(Date.now() - 5 * 60_000).toISOString(),
    ends_at: "0001-01-01T00:00:00Z",
    state: "active",
    labels: {
      alertname: "PathPacketLoss",
      severity: "critical",
      source: "brazil-north",
      target: "paris-core",
    },
    summary: "Elevated packet loss",
    description: null,
    ...overrides,
  } as AlertSummary;
}

describe("AlertRow", () => {
  it("renders alert name, severity, started-at, and summary", () => {
    render(<AlertRow alert={mkAlert()} alertmanagerBaseUrl={null} />);
    expect(
      screen.getByRole("heading", { name: /PathPacketLoss/i }),
    ).toBeInTheDocument();
    expect(screen.getByText(/critical/i)).toBeInTheDocument();
    expect(screen.getByText(/5 minutes ago/i)).toBeInTheDocument();
    expect(screen.getByText(/elevated packet loss/i)).toBeInTheDocument();
  });

  it("renders source & target chips when present", () => {
    render(<AlertRow alert={mkAlert()} alertmanagerBaseUrl={null} />);
    expect(screen.getByText("brazil-north")).toBeInTheDocument();
    expect(screen.getByText("paris-core")).toBeInTheDocument();
  });

  it("hides the Alertmanager link when no base URL is configured", () => {
    render(<AlertRow alert={mkAlert()} alertmanagerBaseUrl={null} />);
    expect(
      screen.queryByRole("link", { name: /view in alertmanager/i }),
    ).toBeNull();
  });

  it("builds the Alertmanager link when a base URL is configured", () => {
    render(
      <AlertRow
        alert={mkAlert()}
        alertmanagerBaseUrl="https://am.example/"
      />,
    );
    const link = screen.getByRole("link", { name: /view in alertmanager/i });
    expect(link).toHaveAttribute(
      "href",
      expect.stringContaining("/#/alerts?filter="),
    );
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute(
      "rel",
      expect.stringContaining("noopener"),
    );
  });

  it("falls back to '(unnamed alert)' when alertname is missing", () => {
    render(
      <AlertRow
        alert={mkAlert({ labels: { severity: "info" } })}
        alertmanagerBaseUrl={null}
      />,
    );
    expect(
      screen.getByRole("heading", { name: /unnamed alert/i }),
    ).toBeInTheDocument();
  });
});
