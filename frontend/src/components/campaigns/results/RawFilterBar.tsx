/**
 * Filter chips for the Raw tab.
 *
 * Three segmented controls — `resolution_state`, `protocol`, `kind` — each
 * writing back to the URL via the plain-object merge pattern. Clearing a
 * chip sets its search param to `undefined`; `campaignDetailSearchSchema`'s
 * per-field `.catch(() => undefined)` normalises that out of the URL without
 * stomping sibling params.
 *
 * Kind enumerates all three backend values — a campaign-kind pair can
 * legitimately reuse a 24 h-old `detail_ping` measurement, so the kind
 * column on the row reflects the measurement's kind, not the pair's.
 */

import type {
  MeasurementKind,
  PairResolutionState,
  ProbeProtocol,
} from "@/api/hooks/campaigns";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

// ---------------------------------------------------------------------------
// Chip sets — exhaustive enumeration so a new backend variant surfaces as a
// type error at compile time instead of a silently-missing chip.
// ---------------------------------------------------------------------------

export const RESOLUTION_STATE_VALUES: readonly PairResolutionState[] = [
  "pending",
  "dispatched",
  "reused",
  "succeeded",
  "unreachable",
  "skipped",
] as const;

export const PROTOCOL_VALUES: readonly ProbeProtocol[] = ["icmp", "tcp", "udp"] as const;

export const KIND_VALUES: readonly MeasurementKind[] = [
  "campaign",
  "detail_ping",
  "detail_mtr",
] as const;

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface RawFilterSelection {
  resolution_state: PairResolutionState | undefined;
  protocol: ProbeProtocol | undefined;
  kind: MeasurementKind | undefined;
}

export interface RawFilterBarProps {
  selection: RawFilterSelection;
  onChange: (next: Partial<RawFilterSelection>) => void;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function RawFilterBar({ selection, onChange }: RawFilterBarProps) {
  return (
    <section
      aria-label="Raw measurements filters"
      className="flex flex-wrap items-start gap-4 rounded-md border p-3"
    >
      <ChipGroup
        legend="Resolution state"
        values={RESOLUTION_STATE_VALUES}
        active={selection.resolution_state}
        onSelect={(next) => onChange({ resolution_state: next })}
      />
      <ChipGroup
        legend="Protocol"
        values={PROTOCOL_VALUES}
        active={selection.protocol}
        onSelect={(next) => onChange({ protocol: next })}
      />
      <ChipGroup
        legend="Kind"
        values={KIND_VALUES}
        active={selection.kind}
        onSelect={(next) => onChange({ kind: next })}
      />
    </section>
  );
}

// ---------------------------------------------------------------------------
// Chip group — single-select with an "All" escape hatch that sets the active
// value to `undefined` (the schema normaliser then drops it from the URL).
// ---------------------------------------------------------------------------

interface ChipGroupProps<T extends string> {
  legend: string;
  values: readonly T[];
  active: T | undefined;
  onSelect: (next: T | undefined) => void;
}

function ChipGroup<T extends string>({ legend, values, active, onSelect }: ChipGroupProps<T>) {
  return (
    <fieldset className="flex flex-col gap-1">
      <legend className="text-xs uppercase tracking-wide text-muted-foreground">{legend}</legend>
      <div className="flex flex-wrap gap-1" role="group" aria-label={legend}>
        <Chip
          label="All"
          selected={active === undefined}
          onClick={() => onSelect(undefined)}
          testId={`raw-filter-${slug(legend)}-all`}
        />
        {values.map((value) => (
          <Chip
            key={value}
            label={value}
            selected={active === value}
            onClick={() => onSelect(active === value ? undefined : value)}
            testId={`raw-filter-${slug(legend)}-${value}`}
          />
        ))}
      </div>
    </fieldset>
  );
}

interface ChipProps {
  label: string;
  selected: boolean;
  onClick: () => void;
  testId: string;
}

function Chip({ label, selected, onClick, testId }: ChipProps) {
  return (
    <Button
      type="button"
      size="sm"
      variant={selected ? "default" : "outline"}
      aria-pressed={selected}
      data-testid={testId}
      onClick={onClick}
      className={cn("h-7 px-2 text-xs", selected ? "" : "text-muted-foreground")}
    >
      {label}
    </Button>
  );
}

function slug(legend: string): string {
  return legend.toLowerCase().replace(/\s+/g, "-");
}
