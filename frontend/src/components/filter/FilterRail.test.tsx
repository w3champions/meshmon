import "@testing-library/jest-dom/vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import type { components } from "@/api/schema.gen";
import type { GeoShape } from "@/lib/geo";
import { FilterRail, type FilterValue } from "./FilterRail";

type FacetsResponse = components["schemas"]["FacetsResponse"];

const EMPTY_VALUE: FilterValue = {
  countryCodes: [],
  asns: [],
  networks: [],
  cities: [],
  shapes: [],
};

function baseFacets(): FacetsResponse {
  return {
    countries: [
      { code: "US", name: "United States", count: 12 },
      { code: "DE", name: "Germany", count: 7 },
      { code: "BR", name: "Brazil", count: 3 },
    ],
    asns: [
      { asn: 7922, count: 10 },
      { asn: 15169, count: 6 },
      { asn: 3320, count: 2 },
    ],
    networks: [
      { name: "Comcast", count: 9 },
      { name: "Hetzner", count: 4 },
    ],
    cities: [
      { name: "Seattle", count: 5 },
      { name: "Berlin", count: 3 },
    ],
  };
}

// Expand one facet group to reveal its options. We scope to <summary> so we
// don't collide with repeated facet titles appearing inside expanded content.
function findSummary(title: RegExp): HTMLElement {
  const summaries = Array.from(document.querySelectorAll("summary"));
  const match = summaries.find((el) => title.test((el.textContent ?? "").trim()));
  if (!match) {
    const seen = summaries.map((el) => JSON.stringify(el.textContent)).join(", ");
    throw new Error(`No summary matching ${title}; saw: ${seen}`);
  }
  return match as HTMLElement;
}

async function openGroup(user: ReturnType<typeof userEvent.setup>, title: RegExp) {
  await user.click(findSummary(title));
}

describe("FilterRail", () => {
  test("renders every filter group header", () => {
    render(<FilterRail value={EMPTY_VALUE} onChange={() => {}} facets={baseFacets()} />);
    expect(findSummary(/Country/)).toBeInTheDocument();
    expect(findSummary(/ASN/)).toBeInTheDocument();
    expect(findSummary(/Network/)).toBeInTheDocument();
    expect(findSummary(/City/)).toBeInTheDocument();
    expect(findSummary(/Name/)).toBeInTheDocument();
    expect(findSummary(/IP Filter/)).toBeInTheDocument();
    expect(findSummary(/Map shapes/)).toBeInTheDocument();
  });

  test("toggling a country calls onChange with the code added to countryCodes", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<FilterRail value={EMPTY_VALUE} onChange={onChange} facets={baseFacets()} />);
    await openGroup(user, /^Country/);
    await user.click(screen.getByRole("button", { name: /germany/i }));
    expect(onChange).toHaveBeenCalledTimes(1);
    const next = onChange.mock.calls[0][0] as FilterValue;
    expect(next.countryCodes).toEqual(["DE"]);
    expect(next).not.toBe(EMPTY_VALUE);
  });

  test("caps facet list at 50 entries after search filter", async () => {
    const user = userEvent.setup();
    const big: FacetsResponse = {
      ...baseFacets(),
      cities: Array.from({ length: 75 }, (_, i) => ({
        name: `City${String(i).padStart(3, "0")}`,
        count: 100 - i,
      })),
    };
    render(<FilterRail value={EMPTY_VALUE} onChange={() => {}} facets={big} />);
    await openGroup(user, /^City/);
    // The cap is applied AFTER any search filter; with an empty query all 75
    // pass the filter, so we expect exactly 50 option buttons plus the
    // truncation hint.
    const options = screen.getAllByRole("button", { name: /^City\d{3}/ });
    expect(options).toHaveLength(50);
    expect(screen.getByText(/Showing top 50 of 75/)).toBeInTheDocument();
  });

  test("shows a graceful empty state when facets are undefined", async () => {
    const user = userEvent.setup();
    render(<FilterRail value={EMPTY_VALUE} onChange={() => {}} facets={undefined} />);
    await openGroup(user, /^Country/);
    const empties = screen.getAllByTestId("facets-empty");
    expect(empties[0]).toHaveTextContent(/facets unavailable/i);
  });

  test("clearing the Name input emits undefined (not empty string)", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <FilterRail
        value={{ ...EMPTY_VALUE, nameSearch: "edge" }}
        onChange={onChange}
        facets={baseFacets()}
      />,
    );
    await openGroup(user, /^Name/);
    const input = screen.getByPlaceholderText(/search display name/i);
    await user.clear(input);
    // userEvent.clear fires one change event with "".
    const lastCall = onChange.mock.calls.at(-1);
    expect(lastCall).toBeDefined();
    const next = lastCall?.[0] as FilterValue;
    expect(next.nameSearch).toBeUndefined();
  });

  test("typing an ASN digit substring filters the ASN options", async () => {
    const user = userEvent.setup();
    render(<FilterRail value={EMPTY_VALUE} onChange={() => {}} facets={baseFacets()} />);
    await openGroup(user, /^ASN/);
    const input = screen.getByPlaceholderText(/search asn/i);
    await user.type(input, "15");
    // 15169 matches, 7922 and 3320 do not.
    expect(screen.getByRole("button", { name: /AS15169/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /AS7922/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /AS3320/ })).not.toBeInTheDocument();
  });

  test("shape section reads 'Open map' with no shapes and 'Edit map' with shapes", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <FilterRail
        value={EMPTY_VALUE}
        onChange={() => {}}
        facets={baseFacets()}
        onOpenMap={() => {}}
      />,
    );
    await openGroup(user, /^Map shapes/);
    expect(screen.getByRole("button", { name: /^open map$/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^clear$/i })).not.toBeInTheDocument();

    const shape: GeoShape = {
      kind: "rectangle",
      sw: [-10, -10],
      ne: [10, 10],
    };
    rerender(
      <FilterRail
        value={{ ...EMPTY_VALUE, shapes: [shape] }}
        onChange={() => {}}
        facets={baseFacets()}
        onOpenMap={() => {}}
      />,
    );
    expect(screen.getByRole("button", { name: /^edit map$/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^clear$/i })).toBeInTheDocument();
  });

  test("clicking the shape Clear button emits shapes: []", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    const shape: GeoShape = {
      kind: "circle",
      center: [0, 0],
      radiusMeters: 1000,
    };
    render(
      <FilterRail
        value={{ ...EMPTY_VALUE, shapes: [shape] }}
        onChange={onChange}
        facets={baseFacets()}
        onOpenMap={() => {}}
      />,
    );
    // The collapsible defaults to open when shapes exist; the content-area
    // Clear button is the one we want.
    await user.click(screen.getByRole("button", { name: /^clear$/i }));
    expect(onChange).toHaveBeenCalledTimes(1);
    const next = onChange.mock.calls[0][0] as FilterValue;
    expect(next.shapes).toEqual([]);
  });

  test("selection badge reflects current picks and opening a group shows item counts", async () => {
    const user = userEvent.setup();
    render(
      <FilterRail
        value={{ ...EMPTY_VALUE, countryCodes: ["US", "DE"] }}
        onChange={() => {}}
        facets={baseFacets()}
      />,
    );
    const countryHeader = findSummary(/Country/);
    expect(within(countryHeader).getByText("2")).toBeInTheDocument();
    await openGroup(user, /^Country/);
    const usButton = screen.getByRole("button", { name: /united states/i });
    expect(usButton).toHaveAttribute("aria-pressed", "true");
  });

  test("Map shapes <details> is uncontrolled — prop changes don't retake control from the user", () => {
    // Regression: previously GroupShell used `open={defaultOpen}`, which made
    // the element controlled. Adding a shape would permanently re-open the
    // section and defeat user collapses. With an uncontrolled <details> +
    // one-shot ref-init, initial state honors the prop at mount; subsequent
    // prop changes MUST NOT change the open attribute.
    const { rerender } = render(
      <FilterRail
        value={EMPTY_VALUE}
        onChange={() => {}}
        facets={baseFacets()}
        onOpenMap={() => {}}
      />,
    );
    const summary = findSummary(/^Map shapes/);
    const details = summary.closest("details") as HTMLDetailsElement;
    expect(details).not.toBeNull();
    // Starts closed with no shapes.
    expect(details.open).toBe(false);

    const shape: GeoShape = {
      kind: "rectangle",
      sw: [-10, -10],
      ne: [10, 10],
    };
    rerender(
      <FilterRail
        value={{ ...EMPTY_VALUE, shapes: [shape] }}
        onChange={() => {}}
        facets={baseFacets()}
        onOpenMap={() => {}}
      />,
    );
    // Adding a shape must NOT force the section open — the user is in control.
    expect(details.open).toBe(false);
  });
});
