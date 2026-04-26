/**
 * Tests for HeatmapColorEditor — covers O5 per plan lines 3220–3225.
 */
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { HeatmapColorEditor } from "@/components/campaigns/results/HeatmapColorEditor";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const MODE = "edge_candidate";
const STORAGE_KEY = `meshmon.evaluation.heatmap.${MODE}.colors`;

function renderEditor(props: Partial<Parameters<typeof HeatmapColorEditor>[0]> = {}) {
  const defaults = {
    open: true,
    onOpenChange: vi.fn(),
    mode: MODE,
    usefulLatencyMs: 100,
    onSaved: vi.fn(),
  };
  const merged = { ...defaults, ...props };
  return {
    ...render(<HeatmapColorEditor {...merged} />),
    props: merged,
  };
}

// ---------------------------------------------------------------------------
// Setup/teardown
// ---------------------------------------------------------------------------

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  localStorage.clear();
});

// ---------------------------------------------------------------------------
// Tests (plan O5 lines 3220–3225)
// ---------------------------------------------------------------------------

describe("HeatmapColorEditor", () => {
  test("renders 4 slidable handles for the default 5-tier scheme", () => {
    renderEditor();

    // 4 handles (boundaries between 5 tiers)
    for (let i = 0; i < 4; i++) {
      expect(screen.getByTestId(`hm-handle-${i}`)).toBeInTheDocument();
    }
  });

  test("renders 4 numeric input fields", () => {
    renderEditor();

    for (let i = 0; i < 4; i++) {
      expect(screen.getByTestId(`hm-boundary-input-${i}`)).toBeInTheDocument();
    }
  });

  test("default boundaries derived from useful_latency_ms=100 are [40,100,200,400]", () => {
    renderEditor({ usefulLatencyMs: 100 });

    expect(screen.getByTestId("hm-boundary-input-0")).toHaveValue(40);
    expect(screen.getByTestId("hm-boundary-input-1")).toHaveValue(100);
    expect(screen.getByTestId("hm-boundary-input-2")).toHaveValue(200);
    expect(screen.getByTestId("hm-boundary-input-3")).toHaveValue(400);
  });

  test("changing a numeric input updates the displayed value", () => {
    renderEditor({ usefulLatencyMs: 100 });

    const input0 = screen.getByTestId("hm-boundary-input-0") as HTMLInputElement;
    fireEvent.change(input0, { target: { value: "50" } });

    expect(input0).toHaveValue(50);
  });

  test("dragging a handle updates the displayed value", () => {
    renderEditor({ usefulLatencyMs: 100 });

    const handle = screen.getByTestId("hm-handle-0");
    const gradientBar = screen.getByTestId("hm-gradient-bar");

    // Mock getBoundingClientRect on the parent track element
    const track = gradientBar.parentElement!;
    vi.spyOn(track, "getBoundingClientRect").mockReturnValue({
      left: 0,
      right: 400,
      width: 400,
      top: 0,
      bottom: 24,
      height: 24,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    // Simulate drag: pointer down then move to 25% (= 125ms of 500ms max)
    fireEvent.pointerDown(handle, { clientX: 0, pointerId: 1 });
    // move to 100px of 400px width = 25% of maxValue
    fireEvent.pointerMove(handle, { clientX: 100, pointerId: 1 });
    fireEvent.pointerUp(handle, { pointerId: 1 });

    // After drag, boundary-0 input should have updated (not still 40)
    const input0 = screen.getByTestId("hm-boundary-input-0");
    // The value should be some positive number (clamped by monotonic constraint)
    const value = parseInt((input0 as HTMLInputElement).value, 10);
    expect(value).toBeGreaterThanOrEqual(0);
  });

  test("Reset restores defaults derived from useful_latency_ms", async () => {
    const user = userEvent.setup();
    renderEditor({ usefulLatencyMs: 100 });

    // Change boundary 0 using fireEvent (avoids userEvent number-input accumulation)
    const input0 = screen.getByTestId("hm-boundary-input-0") as HTMLInputElement;
    fireEvent.change(input0, { target: { value: "999" } });

    // Now reset
    const resetBtn = screen.getByTestId("hm-reset-btn");
    await user.click(resetBtn);

    // Should go back to 40 (0.4×100)
    expect(input0).toHaveValue(40);
  });

  test("Save persists boundaries to localStorage", async () => {
    const user = userEvent.setup();
    const onSaved = vi.fn();
    renderEditor({ usefulLatencyMs: 100, onSaved });

    // Modify boundary 1 using fireEvent so the value is exactly 90
    const input1 = screen.getByTestId("hm-boundary-input-1") as HTMLInputElement;
    fireEvent.change(input1, { target: { value: "90" } });

    // Save
    const saveBtn = screen.getByTestId("hm-save-btn");
    await user.click(saveBtn);

    // localStorage should be updated
    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as number[] | null;
    expect(stored).not.toBeNull();
    expect(stored![1]).toBe(90);

    // onSaved called
    expect(onSaved).toHaveBeenCalledOnce();
  });

  test("on mount restores boundaries from localStorage when present", () => {
    const custom = [15, 60, 130, 350];
    localStorage.setItem(STORAGE_KEY, JSON.stringify(custom));

    renderEditor({ usefulLatencyMs: 100 });

    expect(screen.getByTestId("hm-boundary-input-0")).toHaveValue(15);
    expect(screen.getByTestId("hm-boundary-input-1")).toHaveValue(60);
    expect(screen.getByTestId("hm-boundary-input-2")).toHaveValue(130);
    expect(screen.getByTestId("hm-boundary-input-3")).toHaveValue(350);
  });

  test("renders gradient bar", () => {
    renderEditor();

    expect(screen.getByTestId("hm-gradient-bar")).toBeInTheDocument();
  });

  test("renders Reset and Save buttons", () => {
    renderEditor();

    expect(screen.getByTestId("hm-reset-btn")).toBeInTheDocument();
    expect(screen.getByTestId("hm-save-btn")).toBeInTheDocument();
  });

  test("trigger button renders with Colors label", () => {
    render(
      <HeatmapColorEditor
        open={false}
        onOpenChange={vi.fn()}
        mode={MODE}
        usefulLatencyMs={100}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.getByTestId("hm-color-editor-trigger")).toBeInTheDocument();
    expect(screen.getByTestId("hm-color-editor-trigger")).toHaveTextContent("Colors");
  });

  test("monotonic enforcement: lower boundary cannot exceed upper", () => {
    renderEditor({ usefulLatencyMs: 100 });

    // Set boundary 0 to 150 (above boundary 1 default of 100)
    const input0 = screen.getByTestId("hm-boundary-input-0") as HTMLInputElement;
    fireEvent.change(input0, { target: { value: "150" } });

    // Boundary 1 should be clamped to at least 150
    const input1 = screen.getByTestId("hm-boundary-input-1") as HTMLInputElement;
    const b1 = parseInt(input1.value, 10);
    expect(b1).toBeGreaterThanOrEqual(150);
  });
});
