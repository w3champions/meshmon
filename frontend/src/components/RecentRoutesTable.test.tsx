import { screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { RouteSnapshotSummary } from "@/api/hooks/recent-routes";
import * as recentRoutesHook from "@/api/hooks/recent-routes";
import { RecentRoutesTable } from "@/components/RecentRoutesTable";
import { renderWithProviders } from "@/test/query-wrapper";

vi.mock("@/api/hooks/recent-routes");

// Mock date-fns so tests don't depend on wall-clock time or fake timers
// (fake timers would block TanStack Router's async init inside RouterProvider).
vi.mock("date-fns", async (importOriginal) => {
  const actual = await importOriginal<typeof import("date-fns")>();
  return {
    ...actual,
    formatDistanceToNowStrict: (date: Date, _opts?: unknown) => {
      // Return a deterministic string based on the fixture timestamps
      const iso = date instanceof Date ? date.toISOString() : String(date);
      if (iso.includes("11:59")) return "1 minute ago";
      if (iso.includes("11:58")) return "2 minutes ago";
      if (iso.includes("11:57")) return "3 minutes ago";
      return "some time ago";
    },
  };
});

const ROWS = [
  {
    id: 1,
    source_id: "a",
    target_id: "b",
    protocol: "icmp",
    observed_at: "2026-04-16T11:59:00Z",
    path_summary: null,
  },
  {
    id: 2,
    source_id: "b",
    target_id: "c",
    protocol: "udp",
    observed_at: "2026-04-16T11:58:00Z",
    path_summary: null,
  },
  {
    id: 3,
    source_id: "c",
    target_id: "a",
    protocol: "tcp",
    observed_at: "2026-04-16T11:57:00Z",
    path_summary: null,
  },
];

afterEach(() => {
  vi.clearAllMocks();
});

describe("RecentRoutesTable", () => {
  test("renders all rows with source → target, protocol, and relative time", async () => {
    vi.mocked(recentRoutesHook.useRecentRouteChanges).mockReturnValue({
      data: ROWS,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof recentRoutesHook.useRecentRouteChanges>);

    renderWithProviders(<RecentRoutesTable />);

    // Pairs
    expect(await screen.findByText("a → b")).toBeInTheDocument();
    expect(screen.getByText("b → c")).toBeInTheDocument();
    expect(screen.getByText("c → a")).toBeInTheDocument();

    // Protocols (Tailwind `uppercase` class is visual-only; DOM text remains lowercase)
    expect(screen.getByText("icmp")).toBeInTheDocument();
    expect(screen.getByText("udp")).toBeInTheDocument();
    expect(screen.getByText("tcp")).toBeInTheDocument();

    // Relative time via mocked date-fns
    expect(screen.getByText("1 minute ago")).toBeInTheDocument();
    expect(screen.getByText("2 minutes ago")).toBeInTheDocument();
    expect(screen.getByText("3 minutes ago")).toBeInTheDocument();
  });

  test("renders rows in order provided by the hook", async () => {
    vi.mocked(recentRoutesHook.useRecentRouteChanges).mockReturnValue({
      data: ROWS,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof recentRoutesHook.useRecentRouteChanges>);

    renderWithProviders(<RecentRoutesTable />);

    const pairs = await screen.findAllByText(/→/);
    expect(pairs[0].textContent).toBe("a → b");
    expect(pairs[1].textContent).toBe("b → c");
    expect(pairs[2].textContent).toBe("c → a");
  });

  test("each row view link href encodes source and target params", async () => {
    vi.mocked(recentRoutesHook.useRecentRouteChanges).mockReturnValue({
      data: ROWS,
      isLoading: false,
      isError: false,
    } as ReturnType<typeof recentRoutesHook.useRecentRouteChanges>);

    renderWithProviders(<RecentRoutesTable />);

    const links = await screen.findAllByRole("link", { name: "view" });
    expect(links).toHaveLength(3);

    // TanStack Router renders <a> with the path params substituted
    expect(links[0]).toHaveAttribute("href", "/paths/a/b");
    expect(links[1]).toHaveAttribute("href", "/paths/b/c");
    expect(links[2]).toHaveAttribute("href", "/paths/c/a");
  });

  test("shows skeleton when loading", async () => {
    vi.mocked(recentRoutesHook.useRecentRouteChanges).mockReturnValue({
      data: undefined,
      isLoading: true,
      isError: false,
    } as ReturnType<typeof recentRoutesHook.useRecentRouteChanges>);

    renderWithProviders(<RecentRoutesTable />);

    expect(await screen.findByTestId("recent-routes-skeleton")).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  test("shows 'No recent route changes' for empty array", async () => {
    vi.mocked(recentRoutesHook.useRecentRouteChanges).mockReturnValue({
      data: [] as RouteSnapshotSummary[],
      isLoading: false,
      isError: false,
    } as ReturnType<typeof recentRoutesHook.useRecentRouteChanges>);

    renderWithProviders(<RecentRoutesTable />);

    expect(await screen.findByText("No recent route changes")).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });
});
