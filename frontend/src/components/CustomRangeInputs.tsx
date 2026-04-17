import { useId } from "react";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";

interface CustomRangeInputsProps {
  /** ISO-8601 string (may be empty when the user hasn't chosen a value). */
  from: string;
  /** ISO-8601 string (may be empty). */
  to: string;
  onChange: (next: { from: string; to: string }) => void;
  className?: string;
}

/**
 * Convert an ISO-8601 string into the `YYYY-MM-DDTHH:mm` form
 * `<input type="datetime-local">` accepts. Empty strings pass through.
 */
function isoToDatetimeLocal(iso: string): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}` +
    `T${pad(d.getHours())}:${pad(d.getMinutes())}`
  );
}

/**
 * Interpret the `datetime-local` value (no timezone suffix) as local
 * time and serialise back to an ISO-8601 UTC string. Empty input yields
 * an empty string.
 */
function datetimeLocalToIso(local: string): string {
  if (!local) return "";
  const d = new Date(local);
  if (Number.isNaN(d.getTime())) return "";
  return d.toISOString();
}

export function CustomRangeInputs({ from, to, onChange, className }: CustomRangeInputsProps) {
  // Unique, stable per-instance ids so two pickers on one page don't share
  // `id="range-from"` / `id="range-to"` and break each other's label
  // binding.
  const uid = useId();
  const fromId = `${uid}-from`;
  const toId = `${uid}-to`;

  return (
    <div className={cn("flex flex-wrap items-end gap-2", className)}>
      <div className="flex flex-col gap-1">
        <Label htmlFor={fromId}>From</Label>
        <input
          id={fromId}
          type="datetime-local"
          className="rounded border bg-background p-1 text-sm"
          value={isoToDatetimeLocal(from)}
          onChange={(e) => onChange({ from: datetimeLocalToIso(e.target.value), to })}
        />
      </div>
      <div className="flex flex-col gap-1">
        <Label htmlFor={toId}>To</Label>
        <input
          id={toId}
          type="datetime-local"
          className="rounded border bg-background p-1 text-sm"
          value={isoToDatetimeLocal(to)}
          onChange={(e) => onChange({ from, to: datetimeLocalToIso(e.target.value) })}
        />
      </div>
    </div>
  );
}
