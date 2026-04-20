import { render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import * as campaignsHook from "@/api/hooks/campaigns";
import { SizePreview } from "@/components/campaigns/SizePreview";

vi.mock("@/api/hooks/campaigns");

beforeEach(() => {
  // Default: phase 1 tests don't pass `campaignId`, but the hook still
  // runs (it's disabled internally when id is undefined). Give it a
  // benign idle shape so the component doesn't trip on `preview.data`.
  vi.mocked(campaignsHook.usePreviewDispatchCount).mockReturnValue({
    data: undefined,
    isLoading: false,
    isError: false,
  } as ReturnType<typeof campaignsHook.usePreviewDispatchCount>);
});

afterEach(() => {
  vi.clearAllMocks();
});

function mockPreview(data: { total: number; reusable: number; fresh: number } | undefined) {
  vi.mocked(campaignsHook.usePreviewDispatchCount).mockReturnValue({
    data,
    isLoading: data === undefined,
    isError: false,
  } as ReturnType<typeof campaignsHook.usePreviewDispatchCount>);
}

describe("SizePreview (phase 1 — pre-submit)", () => {
  test("renders the exact product when shapes are inactive", () => {
    render(
      <SizePreview
        sourcesSelected={5}
        approxTotal={10}
        shapesActive={false}
        forceMeasurement={false}
        sizeWarningThreshold={1000}
      />,
    );
    expect(screen.getByText("Expected: 50 measurements")).toBeInTheDocument();
  });

  test("renders the tilde prefix when shapes are active", () => {
    render(
      <SizePreview
        sourcesSelected={5}
        approxTotal={10}
        shapesActive={true}
        forceMeasurement={false}
        sizeWarningThreshold={1000}
      />,
    );
    expect(screen.getByText("Expected: ~50 measurements")).toBeInTheDocument();
  });
});

describe("SizePreview (phase 2 — post-submit)", () => {
  test("renders total / reusable / fresh split when campaignId is set", () => {
    mockPreview({ total: 100, reusable: 30, fresh: 70 });

    render(
      <SizePreview
        sourcesSelected={5}
        approxTotal={0}
        shapesActive={false}
        campaignId="abc"
        forceMeasurement={false}
        sizeWarningThreshold={1000}
      />,
    );

    expect(
      screen.getByText(/Expected: 100 measurements \(30 reusable from last 24 h, 70 new\)\./),
    ).toBeInTheDocument();
  });

  test("renders `{fresh} = {total}` + footnote when forceMeasurement on and reusable > 0", () => {
    mockPreview({ total: 100, reusable: 30, fresh: 70 });

    render(
      <SizePreview
        sourcesSelected={5}
        approxTotal={0}
        shapesActive={false}
        campaignId="abc"
        forceMeasurement={true}
        sizeWarningThreshold={1000}
      />,
    );

    expect(screen.getByText("Expected: 70 = 100 measurements")).toBeInTheDocument();
    expect(
      screen.getByText(/Force measurement is on — reusable count shown as zero\./),
    ).toBeInTheDocument();
  });

  test("calls onThresholdExceeded once when fresh > threshold", () => {
    mockPreview({ total: 1001, reusable: 0, fresh: 1001 });
    const onThresholdExceeded = vi.fn();

    const { rerender } = render(
      <SizePreview
        sourcesSelected={5}
        approxTotal={0}
        shapesActive={false}
        campaignId="abc"
        forceMeasurement={false}
        sizeWarningThreshold={1000}
        onThresholdExceeded={onThresholdExceeded}
      />,
    );

    expect(onThresholdExceeded).toHaveBeenCalledTimes(1);

    // Re-render with the same crossed state: guard must not re-fire.
    rerender(
      <SizePreview
        sourcesSelected={5}
        approxTotal={0}
        shapesActive={false}
        campaignId="abc"
        forceMeasurement={false}
        sizeWarningThreshold={1000}
        onThresholdExceeded={onThresholdExceeded}
      />,
    );
    expect(onThresholdExceeded).toHaveBeenCalledTimes(1);
  });

  test("does NOT fire onThresholdExceeded when fresh === threshold (strict >)", () => {
    mockPreview({ total: 1000, reusable: 0, fresh: 1000 });
    const onThresholdExceeded = vi.fn();

    render(
      <SizePreview
        sourcesSelected={5}
        approxTotal={0}
        shapesActive={false}
        campaignId="abc"
        forceMeasurement={false}
        sizeWarningThreshold={1000}
        onThresholdExceeded={onThresholdExceeded}
      />,
    );
    expect(onThresholdExceeded).not.toHaveBeenCalled();
  });
});
