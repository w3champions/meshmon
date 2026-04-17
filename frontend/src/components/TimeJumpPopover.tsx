import type { ReactNode } from "react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

const MS_MIN = 60 * 1_000;
const MS_HOUR = 60 * MS_MIN;
const MS_DAY = 24 * MS_HOUR;

const QUICK_JUMPS: Array<{ label: string; offsetMs: number }> = [
  { label: "-1d", offsetMs: -MS_DAY },
  { label: "-1h", offsetMs: -MS_HOUR },
  { label: "-15m", offsetMs: -15 * MS_MIN },
  { label: "-5m", offsetMs: -5 * MS_MIN },
  { label: "-1m", offsetMs: -MS_MIN },
  { label: "+1m", offsetMs: MS_MIN },
  { label: "+5m", offsetMs: 5 * MS_MIN },
  { label: "+15m", offsetMs: 15 * MS_MIN },
  { label: "+1h", offsetMs: MS_HOUR },
];

export interface TimeJumpPopoverProps {
  anchorTimeMs: number;
  otherMarkerMs: number;
  side: "A" | "B";
  onRequestJump(targetTimeMs: number): void;
  children: ReactNode;
}

export function TimeJumpPopover({
  anchorTimeMs,
  otherMarkerMs,
  side,
  onRequestJump,
  children,
}: TimeJumpPopoverProps) {
  const [open, setOpen] = useState(false);
  const [customValue, setCustomValue] = useState(defaultDatetimeLocal(anchorTimeMs));

  const wouldCross = (targetMs: number) =>
    side === "A" ? targetMs >= otherMarkerMs : targetMs <= otherMarkerMs;

  const fireJump = (targetMs: number) => {
    onRequestJump(targetMs);
    setOpen(false);
  };

  const applyCustom = () => {
    const parsed = parseDatetimeLocalAsUtc(customValue);
    if (parsed !== undefined && !wouldCross(parsed)) fireJump(parsed);
  };

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>{children}</PopoverTrigger>
      <PopoverContent className="w-64 p-3" align="start">
        <div className="mb-1 text-xs uppercase tracking-wide text-muted-foreground">
          Quick jumps
        </div>
        <div className="mb-3 grid grid-cols-3 gap-1">
          {QUICK_JUMPS.map((jump) => {
            const target = anchorTimeMs + jump.offsetMs;
            const disabled = wouldCross(target);
            return (
              <Button
                key={jump.label}
                type="button"
                size="sm"
                variant="outline"
                disabled={disabled}
                title={disabled ? (side === "A" ? "would cross B" : "would cross A") : undefined}
                onClick={() => fireJump(target)}
              >
                {jump.label}
              </Button>
            );
          })}
        </div>
        <div className="flex items-end gap-2 border-t pt-3">
          <div className="flex-1">
            <Label htmlFor="jump-to" className="text-[0.65rem] uppercase tracking-wide">
              Jump to (UTC)
            </Label>
            <Input
              id="jump-to"
              type="datetime-local"
              value={customValue}
              onChange={(e) => setCustomValue(e.target.value)}
              className="font-mono text-xs"
            />
          </div>
          <Button type="button" size="sm" onClick={applyCustom}>
            Go
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  );
}

function pad2(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

function defaultDatetimeLocal(ms: number): string {
  const d = new Date(ms);
  return `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}T${pad2(
    d.getUTCHours(),
  )}:${pad2(d.getUTCMinutes())}`;
}

function parseDatetimeLocalAsUtc(value: string): number | undefined {
  // datetime-local returns "YYYY-MM-DDTHH:MM" in *local* time, but we treat
  // the typed value as UTC for consistency with the rest of the compare page.
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(value);
  if (!m) return undefined;
  const [, y, mo, d, h, mi] = m;
  return Date.UTC(Number(y), Number(mo) - 1, Number(d), Number(h), Number(mi), 0);
}
