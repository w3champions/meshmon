import { screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import * as alertsHook from "@/api/hooks/alerts";
import { AlertSummaryStrip } from "@/components/AlertSummaryStrip";
import { renderWithProviders } from "@/test/query-wrapper";

vi.mock("@/api/hooks/alerts");

afterEach(() => {
  vi.clearAllMocks();
});

describe("AlertSummaryStrip", () => {
  test("shows critical and warning badges and 'View all' link when there are alerts", async () => {
    vi.mocked(alertsHook.useAlertSummary).mockReturnValue({
      data: { critical: 2, warning: 1, info: 0, total: 3 },
      isLoading: false,
      isError: false,
    });

    renderWithProviders(<AlertSummaryStrip />);

    expect(await screen.findByText("2 critical")).toBeInTheDocument();
    expect(screen.getByText("1 warning")).toBeInTheDocument();
    expect(screen.getByText("View all")).toBeInTheDocument();
  });

  test("does not render info badge when info count is zero", async () => {
    vi.mocked(alertsHook.useAlertSummary).mockReturnValue({
      data: { critical: 2, warning: 1, info: 0, total: 3 },
      isLoading: false,
      isError: false,
    });

    renderWithProviders(<AlertSummaryStrip />);

    await screen.findByText("2 critical");
    expect(screen.queryByText(/info/)).not.toBeInTheDocument();
  });

  test("shows 'No active alerts' when total is 0", async () => {
    vi.mocked(alertsHook.useAlertSummary).mockReturnValue({
      data: { critical: 0, warning: 0, info: 0, total: 0 },
      isLoading: false,
      isError: false,
    });

    renderWithProviders(<AlertSummaryStrip />);

    expect(await screen.findByText("No active alerts")).toBeInTheDocument();
    expect(screen.queryByText("View all")).not.toBeInTheDocument();
  });

  test("shows skeleton when loading", async () => {
    vi.mocked(alertsHook.useAlertSummary).mockReturnValue({
      data: { critical: 0, warning: 0, info: 0, total: 0 },
      isLoading: true,
      isError: false,
    });

    renderWithProviders(<AlertSummaryStrip />);

    expect(await screen.findByTestId("alert-summary-skeleton")).toBeInTheDocument();
    expect(screen.queryByText("No active alerts")).not.toBeInTheDocument();
  });
});
