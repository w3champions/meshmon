import "@testing-library/jest-dom/vitest";
import { cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { renderWithQuery } from "@/test/query-wrapper";
import type { LegDto } from "./RouteLegRow";
import { RouteLegRow } from "./RouteLegRow";

afterEach(cleanup);

function makeLeg(overrides: Partial<LegDto> = {}): LegDto {
  return {
    from_id: "198.51.100.1",
    from_kind: "candidate",
    to_id: "agent-a",
    to_kind: "agent",
    rtt_ms: 10,
    stddev_ms: 1,
    loss_ratio: 0,
    source: "vm_continuous",
    was_substituted: false,
    ...overrides,
  };
}

describe("RouteLegRow", () => {
  it("renders '← reverse-substituted (ingress block detected)' chip when was_substituted=true", () => {
    renderWithQuery(
      <RouteLegRow leg={makeLeg({ was_substituted: true })} lossThresholdRatio={0.1} />,
    );
    expect(screen.getByText("← reverse-substituted (ingress block detected)")).toBeInTheDocument();
  });

  it("renders '← symmetric reuse' chip when source=symmetric_reuse and !was_substituted", () => {
    renderWithQuery(
      <RouteLegRow
        leg={makeLeg({ source: "symmetric_reuse", was_substituted: false })}
        lossThresholdRatio={0.1}
      />,
    );
    expect(screen.getByText("← symmetric reuse")).toBeInTheDocument();
  });

  it("renders no chip when source=vm_continuous and !was_substituted", () => {
    renderWithQuery(
      <RouteLegRow
        leg={makeLeg({ source: "vm_continuous", was_substituted: false })}
        lossThresholdRatio={0.1}
      />,
    );
    expect(screen.queryByText("← reverse-substituted (ingress block detected)")).toBeNull();
    expect(screen.queryByText("← symmetric reuse")).toBeNull();
  });

  it("renders no chip when source=active_probe and !was_substituted", () => {
    renderWithQuery(
      <RouteLegRow
        leg={makeLeg({ source: "active_probe", was_substituted: false })}
        lossThresholdRatio={0.1}
      />,
    );
    expect(screen.queryByText("← reverse-substituted (ingress block detected)")).toBeNull();
    expect(screen.queryByText("← symmetric reuse")).toBeNull();
  });

  it("renders BOTH substituted + 'exceeds loss threshold' chips when leg's substituted loss > loss_threshold_ratio", () => {
    renderWithQuery(
      <RouteLegRow
        leg={makeLeg({ was_substituted: true, loss_ratio: 0.5 })}
        lossThresholdRatio={0.1}
      />,
    );
    expect(screen.getByText("← reverse-substituted (ingress block detected)")).toBeInTheDocument();
    expect(screen.getByText("exceeds loss threshold")).toBeInTheDocument();
  });
});
