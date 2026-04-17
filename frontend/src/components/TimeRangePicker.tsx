import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { TimeRangeKey } from "@/lib/time-range";

const LABELS: Record<TimeRangeKey, string> = {
  "1h": "Last 1 hour",
  "6h": "Last 6 hours",
  "24h": "Last 24 hours",
  "7d": "Last 7 days",
  "30d": "Last 30 days",
  "2y": "Last 2 years",
  custom: "Custom",
};

export interface TimeRangePickerValue {
  range: TimeRangeKey;
  from?: string;
  to?: string;
}

interface TimeRangePickerProps {
  value: TimeRangeKey;
  onChange: (next: TimeRangePickerValue) => void;
  className?: string;
}

export function TimeRangePicker({ value, onChange, className }: TimeRangePickerProps) {
  return (
    <Select value={value} onValueChange={(next) => onChange({ range: next as TimeRangeKey })}>
      <SelectTrigger className={className} aria-label="Time range">
        <SelectValue>{LABELS[value]}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        {(Object.keys(LABELS) as TimeRangeKey[])
          .filter((k) => k !== "custom")
          .map((k) => (
            <SelectItem key={k} value={k}>
              {LABELS[k]}
            </SelectItem>
          ))}
      </SelectContent>
    </Select>
  );
}
