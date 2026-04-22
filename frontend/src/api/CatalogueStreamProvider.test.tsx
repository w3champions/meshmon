import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import CatalogueStreamProvider from "@/api/CatalogueStreamProvider";

vi.mock("@/api/hooks/catalogue-stream", () => ({
  useCatalogueStream: vi.fn(),
}));

import { useCatalogueStream } from "@/api/hooks/catalogue-stream";

afterEach(() => {
  vi.clearAllMocks();
});

describe("CatalogueStreamProvider", () => {
  test("calls useCatalogueStream exactly once on mount", () => {
    render(
      <CatalogueStreamProvider>
        <span>child</span>
      </CatalogueStreamProvider>,
    );

    expect(useCatalogueStream).toHaveBeenCalledTimes(1);
  });

  test("renders its children verbatim", () => {
    render(
      <CatalogueStreamProvider>
        <span data-testid="child">child</span>
      </CatalogueStreamProvider>,
    );

    expect(screen.getByTestId("child")).toBeInTheDocument();
  });
});
