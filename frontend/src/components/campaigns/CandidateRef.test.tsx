import "@testing-library/jest-dom/vitest";
import { cleanup, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderWithQuery } from "@/test/query-wrapper";
import { CatalogueDrawerOverlay } from "@/components/catalogue/CatalogueDrawerOverlay";
import { CandidateRef } from "./CandidateRef";
import type { CandidateRefData } from "./CandidateRef";

// Stub EventSource for IpHostnameProvider
class NoopEventSource {
  constructor(public url: string) {}
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {}
}

vi.stubGlobal("EventSource", NoopEventSource);

// Mock useNavigate so tests don't need a full router tree
const navigate = vi.fn();
vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return { ...actual, useNavigate: () => navigate };
});

afterEach(() => {
  cleanup();
  navigate.mockReset();
});

const minimal: CandidateRefData = {
  ip: "198.51.100.7",
  display_name: "OneProvider FRA",
  city: "Frankfurt",
  country_code: "DE",
  asn: 4711,
  network_operator: "OneProvider",
  is_mesh_member: false,
};

/**
 * Wraps `ui` with CatalogueDrawerOverlay inside the shared provider tree.
 * The Overlay uses `useCatalogueDrawer` internally, so it must be within the
 * same QueryClient + IpHostnameProvider tree that `renderWithProviders` sets up.
 */
function render(ui: React.ReactElement) {
  return renderWithQuery(<CatalogueDrawerOverlay>{ui}</CatalogueDrawerOverlay>);
}

describe("CandidateRef", () => {
  it("compact mode shows display_name + IP + city/ASN/operator chips", () => {
    render(<CandidateRef mode="compact" data={minimal} />);
    expect(screen.getByText("OneProvider FRA")).toBeInTheDocument();
    expect(screen.getByText("198.51.100.7")).toBeInTheDocument();
    expect(screen.getByText(/Frankfurt/)).toBeInTheDocument();
    expect(screen.getByText(/AS4711/)).toBeInTheDocument();
    // network_operator chip — getAll since display_name also contains "OneProvider"
    const allOneProvider = screen.getAllByText(/OneProvider/);
    expect(allOneProvider.length).toBeGreaterThanOrEqual(1);
  });

  it("compact mode shows Open icon button on hover", async () => {
    render(<CandidateRef mode="compact" data={minimal} />);
    const button = screen.getByRole("button", { name: /open in catalogue/i });
    expect(button).toBeInTheDocument();
  });

  it("header mode shows full enrichment + Open in catalogue button", () => {
    render(<CandidateRef mode="header" data={minimal} />);
    expect(screen.getByRole("button", { name: /open in catalogue/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /open agent detail/i })).toBeNull();
  });

  it("header mode shows agent-detail button when is_mesh_member=true", () => {
    render(
      <CandidateRef
        mode="header"
        data={{ ...minimal, is_mesh_member: true, agent_id: "frankfurt" }}
      />,
    );
    expect(screen.getByRole("button", { name: /open agent detail/i })).toBeInTheDocument();
  });

  it("Open agent detail navigates to /agents/:id", async () => {
    const user = userEvent.setup();
    render(
      <CandidateRef
        mode="header"
        data={{ ...minimal, is_mesh_member: true, agent_id: "frankfurt" }}
      />,
    );
    const button = screen.getByRole("button", { name: /open agent detail/i });
    await user.click(button);
    expect(navigate).toHaveBeenCalledWith({ to: "/agents/$id", params: { id: "frankfurt" } });
  });

  it("inline mode shows display_name as a clickable element", () => {
    render(<CandidateRef mode="inline" data={minimal} />);
    const link = screen.getByRole("button", { name: "OneProvider FRA" });
    expect(link).toBeInTheDocument();
  });
});
