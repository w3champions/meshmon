import { CustomRangeInputs } from "@/components/CustomRangeInputs";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { rangeBounds, type TimeRangeKey } from "@/lib/time-range";

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
  /** ISO-8601 `from` bound — only meaningful when range is 'custom'. */
  from?: string;
  /** ISO-8601 `to` bound — only meaningful when range is 'custom'. */
  to?: string;
  onChange: (next: TimeRangePickerValue) => void;
  className?: string;
}

export function TimeRangePicker({
  value,
  from = "",
  to = "",
  onChange,
  className,
}: TimeRangePickerProps) {
  const handlePresetChange = (next: string) => {
    const range = next as TimeRangeKey;
    if (range === "custom") {
      // Seed from/to with the current preset's bounds (or 24h as a universal
      // fallback when already on 'custom' without bounds) so the router
      // schema — which rejects empty strings — accepts the switch. Without
      // this the picker silently refuses to move off the prior preset.
      if (from && to) {
        onChange({ range, from, to });
        return;
      }
      const seed = value === "custom" ? "24h" : value;
      const bounds = rangeBounds(seed);
      onChange({
        range,
        from: bounds.from.toISOString(),
        to: bounds.to.toISOString(),
      });
      return;
    }
    onChange({ range });
  };

  return (
    <div className={className}>
      <Select value={value} onValueChange={handlePresetChange}>
        <SelectTrigger aria-label="Time range">
          <SelectValue>{LABELS[value]}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          {(Object.keys(LABELS) as TimeRangeKey[]).map((k) => (
            <SelectItem key={k} value={k}>
              {LABELS[k]}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {value === "custom" && (
        <CustomRangeInputs
          className="mt-2"
          from={from}
          to={to}
          onChange={({ from: f, to: t }) => onChange({ range: "custom", from: f, to: t })}
        />
      )}
    </div>
  );
}
