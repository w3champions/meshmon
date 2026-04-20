import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { COUNTRIES } from "@/lib/countries";

/**
 * Atomic `{code, name}` pair emitted by [`CountryPicker`]. Both the
 * `PasteStaging` bulk-metadata panel and any future caller that needs
 * to submit a paired `country_code` + `country_name` pull the values
 * from this shape together — the backend's paste handler rejects a
 * half-supplied pair, so the component never exposes one half in
 * isolation.
 */
export interface CountryValue {
  code: string;
  name: string;
}

export interface CountryPickerProps {
  /** Current selection, or `null` for no country. */
  value: CountryValue | null;
  /** Emitted when the operator picks a country or clears the field. */
  onChange(next: CountryValue | null): void;
  /** Optional id so a sibling `Label` can target the trigger. */
  id?: string;
  /** Disable the control (e.g. while a mutation is in flight). */
  disabled?: boolean;
  /** Placeholder shown on the empty selection. */
  placeholder?: string;
}

// Radix Select forbids an empty string on `SelectItem`, so the
// "clear" row uses a sentinel that the onValueChange maps back to
// `null`. Must not collide with any real ISO code.
const NONE_SENTINEL = "__none__";

export function CountryPicker({
  value,
  onChange,
  id,
  disabled,
  placeholder = "Select a country…",
}: CountryPickerProps) {
  return (
    <Select
      // Passing `undefined` keeps the trigger in the placeholder state
      // so empty selections read as "Select a country…" rather than
      // matching the `__none__` sentinel's label. Once the operator
      // picks an item the controlled form activates.
      value={value ? value.code : undefined}
      onValueChange={(next) => {
        if (next === NONE_SENTINEL) {
          onChange(null);
          return;
        }
        const picked = COUNTRIES.find((c) => c.code === next);
        if (picked) {
          onChange({ code: picked.code, name: picked.name });
        }
      }}
      disabled={disabled}
    >
      <SelectTrigger id={id}>
        <SelectValue placeholder={placeholder} />
      </SelectTrigger>
      <SelectContent className="max-h-72">
        <SelectItem value={NONE_SENTINEL}>— (none)</SelectItem>
        {COUNTRIES.map((c) => (
          <SelectItem key={c.code} value={c.code}>
            {c.name} ({c.code})
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
