/**
 * HeatmapColorEditor — popover for editing the 4 tier-boundary values that
 * determine cell colours in the HeatmapTab.
 *
 * Renders 4 numeric handles on a horizontal gradient bar representing the
 * 5-tier colour scheme. Each boundary separates adjacent tiers. Handles can
 * be adjusted via numeric inputs and are kept in monotonically increasing
 * order (handle N+1 ≥ handle N). Reset restores the defaults derived from
 * `useful_latency_ms`. Save persists to localStorage at
 * `meshmon.evaluation.heatmap.{mode}.colors`.
 */

import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface HeatmapColorEditorProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Evaluation mode — used as the localStorage key namespace. */
  mode: string;
  /** `evaluation.useful_latency_ms` from which defaults derive. */
  usefulLatencyMs: number | null;
  /** Called when boundaries are saved to localStorage. */
  onSaved: () => void;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function defaultBoundaries(usefulLatencyMs: number | null): [number, number, number, number] {
  const T = usefulLatencyMs ?? 80;
  return [0.4 * T, T, 2 * T, 4 * T];
}

function storageKey(mode: string): string {
  return `meshmon.evaluation.heatmap.${mode}.colors`;
}

function readStored(mode: string): [number, number, number, number] | null {
  try {
    const raw = localStorage.getItem(storageKey(mode));
    if (!raw) return null;
    const parsed = JSON.parse(raw) as unknown;
    if (
      Array.isArray(parsed) &&
      parsed.length === 4 &&
      parsed.every((v) => typeof v === "number")
    ) {
      return parsed as [number, number, number, number];
    }
  } catch {
    // ignore
  }
  return null;
}

/** Clamp a boundary update while maintaining monotonic order. */
function clampBoundaries(
  current: [number, number, number, number],
  index: number,
  value: number,
): [number, number, number, number] {
  const next = [...current] as [number, number, number, number];
  next[index] = value;

  // Enforce monotonic order: push neighbours if needed
  for (let i = index - 1; i >= 0; i--) {
    if (next[i]! > next[i + 1]!) next[i] = next[i + 1]!;
  }
  for (let i = index + 1; i < 4; i++) {
    if (next[i]! < next[i - 1]!) next[i] = next[i - 1]!;
  }

  return next;
}

// ---------------------------------------------------------------------------
// Tier gradient preview bar
// ---------------------------------------------------------------------------

const TIER_COLORS = [
  "var(--hm-tier-1)", // < b[0]
  "var(--hm-tier-2)", // b[0]..b[1]
  "var(--hm-tier-3)", // b[1]..b[2]
  "var(--hm-tier-4)", // b[2]..b[3]
  "var(--hm-tier-5)", // > b[3]
];

// ---------------------------------------------------------------------------
// Draggable handle
// ---------------------------------------------------------------------------

interface HandleProps {
  index: number;
  value: number;
  boundaries: [number, number, number, number];
  maxValue: number;
  onChange: (index: number, value: number) => void;
}

function Handle({ index, value, boundaries, maxValue, onChange }: HandleProps) {
  const trackRef = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);

  const fraction = maxValue > 0 ? Math.min(value / maxValue, 1) : 0;
  const left = `${(fraction * 100).toFixed(1)}%`;

  const getValueFromEvent = (clientX: number): number => {
    const track = trackRef.current?.closest("[data-hm-track]") as HTMLDivElement | null;
    if (!track) return value;
    const rect = track.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
    return Math.round(ratio * maxValue);
  };

  const handlePointerDown = (e: React.PointerEvent) => {
    dragging.current = true;
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const handlePointerMove = (e: React.PointerEvent) => {
    if (!dragging.current) return;
    const newVal = getValueFromEvent(e.clientX);
    onChange(index, newVal);
  };

  const handlePointerUp = () => {
    dragging.current = false;
  };

  void boundaries; // used by parent to compute positions

  return (
    <div
      data-testid={`hm-handle-${index}`}
      className="absolute top-0 -translate-x-1/2 cursor-col-resize"
      style={{ left }}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      onPointerCancel={handlePointerUp}
      role="slider"
      aria-label={`Boundary ${index + 1}`}
      aria-valuenow={Math.round(value)}
      aria-valuemin={0}
      aria-valuemax={maxValue}
      tabIndex={0}
      onKeyDown={(e) => {
        const step = e.shiftKey ? 10 : 1;
        if (e.key === "ArrowRight") onChange(index, value + step);
        else if (e.key === "ArrowLeft") onChange(index, Math.max(0, value - step));
      }}
    >
      <div
        ref={trackRef}
        className="h-6 w-2 rounded-sm border-2 border-white shadow-md bg-foreground"
        title={`${Math.round(value)} ms`}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function HeatmapColorEditor({
  open,
  onOpenChange,
  mode,
  usefulLatencyMs,
  onSaved,
}: HeatmapColorEditorProps) {
  const defaults = defaultBoundaries(usefulLatencyMs);
  // Initialise from localStorage or defaults
  const [boundaries, setBoundaries] = useState<[number, number, number, number]>(
    () => readStored(mode) ?? defaults,
  );

  // Re-read from storage when mode changes
  useEffect(() => {
    setBoundaries(readStored(mode) ?? defaultBoundaries(usefulLatencyMs));
  }, [mode, usefulLatencyMs]);

  // Maximum axis value: 5× T (to give plenty of room)
  const maxValue = Math.max((usefulLatencyMs ?? 80) * 5, boundaries[3]! * 1.2, 400);

  const handleChange = (index: number, value: number) => {
    setBoundaries((prev) => clampBoundaries(prev, index, value));
  };

  const handleInputChange = (index: number, raw: string) => {
    const num = parseFloat(raw);
    if (!isNaN(num) && num >= 0) {
      setBoundaries((prev) => clampBoundaries(prev, index, num));
    }
  };

  const handleReset = () => {
    setBoundaries(defaults as [number, number, number, number]);
  };

  const handleSave = () => {
    localStorage.setItem(storageKey(mode), JSON.stringify(boundaries));
    onSaved();
    onOpenChange(false);
  };

  // Gradient stops for the preview bar
  const segments = TIER_COLORS.map((color, i) => {
    const start = i === 0 ? 0 : (boundaries[i - 1]! / maxValue) * 100;
    const end = i === 4 ? 100 : (boundaries[i]! / maxValue) * 100;
    return { color, start, end };
  });

  const gradient = segments
    .map(({ color, start, end }) => `${color} ${start.toFixed(1)}% ${end.toFixed(1)}%`)
    .join(", ");

  return (
    <Popover open={open} onOpenChange={onOpenChange}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          size="sm"
          variant="outline"
          className="h-7 px-2 text-xs gap-1"
          data-testid="hm-color-editor-trigger"
          aria-expanded={open}
        >
          🎨 Colors
        </Button>
      </PopoverTrigger>
      <PopoverContent
        className="w-96 p-4"
        data-testid="hm-color-editor-content"
        align="end"
      >
        <div className="flex flex-col gap-4">
          <div>
            <h3 className="text-sm font-semibold mb-1">Tier boundaries (ms)</h3>
            <p className="text-xs text-muted-foreground">
              Drag handles or enter values. Default T = {Math.round(usefulLatencyMs ?? 80)} ms.
            </p>
          </div>

          {/* Gradient bar with draggable handles */}
          <div className="relative" data-hm-track>
            <div
              className="h-6 rounded-md w-full"
              style={{ background: `linear-gradient(to right, ${gradient})` }}
              data-testid="hm-gradient-bar"
            />
            {boundaries.map((val, i) => (
              <Handle
                key={i}
                index={i}
                value={val}
                boundaries={boundaries}
                maxValue={maxValue}
                onChange={handleChange}
              />
            ))}
          </div>

          {/* Tier labels */}
          <div className="flex gap-1 text-xs">
            {["Excellent", "Good", "Warning", "Slow", "Bad"].map((label, i) => (
              <div
                key={i}
                className="flex-1 text-center py-0.5 rounded text-white font-medium truncate"
                style={{ background: TIER_COLORS[i] }}
              >
                {label}
              </div>
            ))}
          </div>

          {/* Numeric inputs */}
          <div className="grid grid-cols-4 gap-2">
            {boundaries.map((val, i) => (
              <div key={i} className="flex flex-col gap-1">
                <label className="text-xs text-muted-foreground" htmlFor={`hm-boundary-${i}`}>
                  B{i + 1}
                </label>
                <input
                  id={`hm-boundary-${i}`}
                  type="number"
                  min={0}
                  step={1}
                  className="w-full rounded border border-input bg-background px-2 py-1 text-xs"
                  value={Math.round(val)}
                  data-testid={`hm-boundary-input-${i}`}
                  onChange={(e) => handleInputChange(i, e.target.value)}
                />
              </div>
            ))}
          </div>

          {/* Actions */}
          <div className="flex gap-2 justify-end">
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={handleReset}
              data-testid="hm-reset-btn"
            >
              Reset
            </Button>
            <Button
              type="button"
              size="sm"
              onClick={handleSave}
              data-testid="hm-save-btn"
            >
              Save
            </Button>
          </div>
        </div>
      </PopoverContent>
    </Popover>
  );
}
