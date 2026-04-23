import { useEffect, useMemo, useState } from "react";
import type { ProbeProtocol } from "@/api/hooks/campaigns";
import {
  type HistoryDestination,
  type HistorySource,
  useHistoryDestinations,
  useHistorySources,
} from "@/api/hooks/history";
import { CustomRangeInputs } from "@/components/CustomRangeInputs";
import { IpHostname } from "@/components/ip-hostname";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { cn } from "@/lib/utils";

export type HistoryRange = "24h" | "7d" | "30d" | "90d" | "custom";

// Keep the tuple in lockstep with the `HistoryRange` union. `satisfies readonly
// HistoryRange[]` forces a build break if a range variant ever exists without a
// key here, which in turn keeps `isHistoryRange` exhaustive.
const RANGE_KEYS = ["24h", "7d", "30d", "90d", "custom"] as const satisfies readonly HistoryRange[];

function isHistoryRange(v: string): v is HistoryRange {
  return (RANGE_KEYS as readonly string[]).includes(v);
}

const RANGE_LABELS: Record<HistoryRange, string> = {
  "24h": "24h",
  "7d": "7d",
  "30d": "30d",
  "90d": "90d",
  custom: "Custom",
};

const ALL_PROTOCOLS: readonly ProbeProtocol[] = ["icmp", "tcp", "udp"] as const;

export interface HistoryPairFiltersValue {
  source?: string;
  destination?: string;
  protocols: readonly ProbeProtocol[];
  range: HistoryRange;
  from?: string;
  to?: string;
}

export interface HistoryPairFiltersProps {
  value: HistoryPairFiltersValue;
  onChange(next: HistoryPairFiltersValue): void;
}

export function HistoryPairFilters({ value, onChange }: HistoryPairFiltersProps) {
  const sources = useHistorySources();
  const sourceMap = useMemo(() => {
    const map = new Map<string, HistorySource>();
    for (const s of sources.data ?? []) map.set(s.source_agent_id, s);
    return map;
  }, [sources.data]);

  const selectedSource = value.source ? sourceMap.get(value.source) : undefined;

  return (
    <div
      className="sticky top-0 z-10 flex flex-wrap items-end gap-3 border-b bg-background/95 p-3 backdrop-blur-sm"
      data-testid="history-pair-filters"
    >
      <SourcePicker
        sources={sources.data ?? []}
        loading={sources.isPending}
        selected={selectedSource}
        fallbackId={value.source}
        onSelect={(id) =>
          onChange({
            ...value,
            source: id,
            // Clear destination when the source changes so stale picks don't
            // silently carry into a new set.
            destination: id === value.source ? value.destination : undefined,
          })
        }
      />
      <DestinationPicker
        source={value.source}
        selectedId={value.destination}
        onSelect={(id) => onChange({ ...value, destination: id })}
      />
      <div className="flex flex-col gap-1">
        <span className="text-xs text-muted-foreground">Protocols</span>
        <ToggleGroup
          type="multiple"
          variant="outline"
          size="sm"
          value={[...value.protocols]}
          onValueChange={(next) =>
            onChange({
              ...value,
              protocols: next.filter((p): p is ProbeProtocol =>
                (ALL_PROTOCOLS as readonly string[]).includes(p),
              ),
            })
          }
          aria-label="Protocols"
        >
          {ALL_PROTOCOLS.map((p) => (
            <ToggleGroupItem key={p} value={p} aria-label={p}>
              {p.toUpperCase()}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </div>
      <div className="flex flex-col gap-1">
        <span className="text-xs text-muted-foreground">Range</span>
        <ToggleGroup
          type="single"
          variant="outline"
          size="sm"
          value={value.range}
          onValueChange={(next) => {
            if (!next) return;
            // Narrow the raw toggle value before trusting it — a future
            // `ToggleGroupItem` added outside the `HistoryRange` union would
            // otherwise slip through and break the router schema later.
            if (!isHistoryRange(next)) return;
            const range = next;
            if (range === "custom") {
              // Seed a 24h window when switching to custom without bounds so
              // the zod schema (which requires from+to for custom) accepts.
              const seededFrom = value.from ?? new Date(Date.now() - 24 * 3_600_000).toISOString();
              const seededTo = value.to ?? new Date().toISOString();
              onChange({ ...value, range, from: seededFrom, to: seededTo });
              return;
            }
            onChange({ ...value, range, from: undefined, to: undefined });
          }}
          aria-label="Time range"
        >
          {(Object.keys(RANGE_LABELS) as HistoryRange[]).map((r) => (
            <ToggleGroupItem key={r} value={r} aria-label={RANGE_LABELS[r]}>
              {RANGE_LABELS[r]}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </div>
      {value.range === "custom" && (
        <CustomRangeInputs
          from={value.from ?? ""}
          to={value.to ?? ""}
          onChange={({ from, to }) => {
            // Drop transient invalid states while editing. `historyPairSearchSchema`
            // rejects empty datetime strings (via `z.string().datetime()`) and
            // requires both bounds for `range=custom`, so propagating an empty
            // field would throw inside `validateSearch` and silently lose the
            // edit. The next complete change takes effect normally.
            if (!from || !to) return;
            onChange({
              ...value,
              range: "custom",
              from,
              to,
            });
          }}
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// SourcePicker
// ---------------------------------------------------------------------------

interface SourcePickerProps {
  sources: readonly HistorySource[];
  loading: boolean;
  selected: HistorySource | undefined;
  /**
   * When a landing URL carries a source id that isn't in the catalogue yet
   * (or was deleted), show the raw id in the trigger so the operator still
   * sees what they picked.
   */
  fallbackId: string | undefined;
  onSelect(id: string): void;
}

function SourcePicker({ sources, loading, selected, fallbackId, onSelect }: SourcePickerProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  // `focusedIndex` drives keyboard navigation via `aria-activedescendant`.
  // `-1` means "no option focused" — the filter input has the real DOM focus
  // throughout (WAI-ARIA filterable-listbox pattern); option focus is
  // virtual, signalled only by the active-descendant relationship + a
  // matching visible style on the row.
  const [focusedIndex, setFocusedIndex] = useState<number>(-1);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return sources;
    return sources.filter(
      (s) =>
        s.source_agent_id.toLowerCase().includes(q) || s.display_name.toLowerCase().includes(q),
    );
  }, [sources, query]);

  // Reset virtual focus when the popover closes; clear it on any change to
  // the filtered identity so a same-length content swap (e.g. a background
  // refetch returning a different source set) can't leave the
  // active-descendant pointing at a stale row and silently select the
  // wrong entry on Enter.
  useEffect(() => {
    if (!open) setFocusedIndex(-1);
  }, [open]);
  useEffect(() => {
    // Touching `.length` keeps biome's useExhaustiveDependencies happy
    // while the real signal we care about is `filtered`'s identity.
    void filtered.length;
    setFocusedIndex(-1);
  }, [filtered]);

  const label = selected ? selected.display_name : fallbackId ? fallbackId : "Pick a source";
  const focusedOption = focusedIndex >= 0 ? filtered[focusedIndex] : undefined;
  const focusedId = focusedOption ? `source-opt-${focusedOption.source_agent_id}` : undefined;

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>): void => {
    if (filtered.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setFocusedIndex((prev) => (prev + 1) % filtered.length);
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setFocusedIndex((prev) => (prev <= 0 ? filtered.length - 1 : prev - 1));
      return;
    }
    if (e.key === "Enter") {
      if (focusedIndex < 0) return;
      e.preventDefault();
      const picked = filtered[focusedIndex];
      if (!picked) return;
      onSelect(picked.source_agent_id);
      setOpen(false);
      setQuery("");
    }
  };

  return (
    <div className="flex flex-col gap-1">
      <span className="text-xs text-muted-foreground">Source</span>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button
            type="button"
            variant="outline"
            role="combobox"
            aria-expanded={open}
            aria-label="Source picker"
            className="w-56 justify-between"
          >
            <span className="truncate">{label}</span>
            <span aria-hidden className="ml-2 text-xs text-muted-foreground">
              ▾
            </span>
          </Button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-80 p-2">
          <Input
            type="search"
            placeholder="Search sources…"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              // Reset virtual focus whenever the filter query changes; the
              // new `filtered` list may not line up with the prior index.
              setFocusedIndex(-1);
            }}
            onKeyDown={handleKeyDown}
            aria-label="Filter sources"
            aria-controls="source-listbox"
            aria-activedescendant={focusedId}
            className="mb-2"
            autoFocus
          />
          {loading ? (
            <p className="px-2 py-1 text-sm text-muted-foreground">Loading…</p>
          ) : filtered.length === 0 ? (
            <p className="px-2 py-1 text-sm text-muted-foreground">No sources match.</p>
          ) : (
            <div
              id="source-listbox"
              role="listbox"
              aria-label="Sources"
              className="max-h-72 overflow-y-auto"
            >
              {filtered.map((s, idx) => {
                const isSel = s.source_agent_id === selected?.source_agent_id;
                const isFocused = idx === focusedIndex;
                return (
                  <button
                    key={s.source_agent_id}
                    id={`source-opt-${s.source_agent_id}`}
                    type="button"
                    role="option"
                    aria-selected={isSel}
                    data-focused={isFocused ? "true" : undefined}
                    // `tabIndex={-1}` because virtual focus stays on the
                    // filter input; clicking still works, but the option
                    // is never a tab stop.
                    tabIndex={-1}
                    onMouseEnter={() => setFocusedIndex(idx)}
                    onClick={() => {
                      onSelect(s.source_agent_id);
                      setOpen(false);
                      setQuery("");
                    }}
                    className={cn(
                      "flex w-full flex-col items-start gap-0.5 rounded px-2 py-1 text-left text-sm hover:bg-accent",
                      isSel && "bg-accent",
                      // `ring` ties the visible highlight to the
                      // `aria-activedescendant` value so sighted keyboard
                      // users see what the a11y tree announces.
                      isFocused && "bg-accent ring-1 ring-ring",
                    )}
                  >
                    <span className="font-medium">{s.display_name}</span>
                    <span className="font-mono text-xs text-muted-foreground">
                      {s.source_agent_id}
                    </span>
                  </button>
                );
              })}
            </div>
          )}
        </PopoverContent>
      </Popover>
    </div>
  );
}

// ---------------------------------------------------------------------------
// DestinationPicker
// ---------------------------------------------------------------------------

interface DestinationPickerProps {
  source: string | undefined;
  selectedId: string | undefined;
  onSelect(ip: string): void;
}

function DestinationPicker({ source, selectedId, onSelect }: DestinationPickerProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  // See `SourcePicker` for the keyboard-navigation contract; the same
  // filterable-listbox pattern applies here.
  const [focusedIndex, setFocusedIndex] = useState<number>(-1);

  // Debounce the query so we don't thrash the backend as the operator types.
  // Destination fetch is gated on source + includes the `q` param so the
  // backend can narrow on catalogue + IP substring.
  const debouncedQuery = useDebounced(query, 200);
  const destinations = useHistoryDestinations(source, debouncedQuery.trim() || undefined);

  const options = useMemo(() => destinations.data ?? [], [destinations.data]);

  // See `SourcePicker` — same rationale: reset on any options identity
  // change so a content swap doesn't leave `aria-activedescendant` pointing
  // at a stale row.
  useEffect(() => {
    if (!open) setFocusedIndex(-1);
  }, [open]);
  useEffect(() => {
    // See `SourcePicker` — `.length` read keeps biome quiet; identity is
    // what we actually react to.
    void options.length;
    setFocusedIndex(-1);
  }, [options]);

  const selected = useMemo(
    () => options.find((d) => d.destination_ip === selectedId),
    [options, selectedId],
  );

  const disabled = !source;

  const triggerLabel = selected
    ? selected.display_name
    : selectedId
      ? selectedId
      : disabled
        ? "Pick a source first"
        : "Pick a destination";

  const focusedOption = focusedIndex >= 0 ? options[focusedIndex] : undefined;
  const focusedId = focusedOption ? `dest-opt-${focusedOption.destination_ip}` : undefined;

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>): void => {
    if (options.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setFocusedIndex((prev) => (prev + 1) % options.length);
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setFocusedIndex((prev) => (prev <= 0 ? options.length - 1 : prev - 1));
      return;
    }
    if (e.key === "Enter") {
      if (focusedIndex < 0) return;
      e.preventDefault();
      const picked = options[focusedIndex];
      if (!picked) return;
      onSelect(picked.destination_ip);
      setOpen(false);
      setQuery("");
    }
  };

  return (
    <div className="flex flex-col gap-1">
      <span className="text-xs text-muted-foreground">Destination</span>
      <Popover
        open={open && !disabled}
        onOpenChange={(next) => {
          if (disabled) return;
          setOpen(next);
        }}
      >
        <PopoverTrigger asChild>
          <Button
            type="button"
            variant="outline"
            role="combobox"
            aria-expanded={open}
            aria-label="Destination picker"
            disabled={disabled}
            className="w-64 justify-between"
          >
            <span className="truncate">{triggerLabel}</span>
            <span aria-hidden className="ml-2 text-xs text-muted-foreground">
              ▾
            </span>
          </Button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-96 p-2">
          <Input
            type="search"
            placeholder="Search destinations…"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              // Reset virtual focus whenever the filter query changes; the
              // debounced fetch will refresh `options` shortly, and any
              // preserved index would point into the stale list.
              setFocusedIndex(-1);
            }}
            onKeyDown={handleKeyDown}
            aria-label="Filter destinations"
            aria-controls="dest-listbox"
            aria-activedescendant={focusedId}
            className="mb-2"
            autoFocus
          />
          {destinations.isPending ? (
            <p className="px-2 py-1 text-sm text-muted-foreground">Loading…</p>
          ) : destinations.isError ? (
            <p className="px-2 py-1 text-sm text-destructive">Failed to load destinations.</p>
          ) : options.length === 0 ? (
            <p className="px-2 py-1 text-sm text-muted-foreground">No destinations match.</p>
          ) : (
            <div
              id="dest-listbox"
              role="listbox"
              aria-label="Destinations"
              className="max-h-72 overflow-y-auto"
            >
              {options.map((d, idx) => (
                <DestinationOption
                  key={d.destination_ip}
                  destination={d}
                  selected={d.destination_ip === selectedId}
                  focused={idx === focusedIndex}
                  onMouseEnter={() => setFocusedIndex(idx)}
                  onClick={() => {
                    onSelect(d.destination_ip);
                    setOpen(false);
                    setQuery("");
                  }}
                />
              ))}
            </div>
          )}
        </PopoverContent>
      </Popover>
    </div>
  );
}

interface DestinationOptionProps {
  destination: HistoryDestination;
  selected: boolean;
  focused: boolean;
  onClick(): void;
  onMouseEnter(): void;
}

function DestinationOption({
  destination,
  selected,
  focused,
  onClick,
  onMouseEnter,
}: DestinationOptionProps) {
  // T42 null tolerance — when the catalogue row has been deleted, the
  // backend leaves `display_name` as the raw IP and every geo/ASN field
  // stays null. Render "raw IP — no metadata" rather than treating it as
  // a rendering bug; it's a supported state.
  const catalogueMissing =
    destination.display_name === destination.destination_ip &&
    destination.city == null &&
    destination.country_code == null &&
    destination.asn == null;

  return (
    <button
      id={`dest-opt-${destination.destination_ip}`}
      type="button"
      role="option"
      aria-selected={selected}
      data-focused={focused ? "true" : undefined}
      // `tabIndex={-1}` because virtual focus stays on the filter input; see
      // `SourcePicker` for the full pattern.
      tabIndex={-1}
      onMouseEnter={onMouseEnter}
      onClick={onClick}
      className={cn(
        "flex w-full flex-col items-start gap-0.5 rounded px-2 py-1 text-left text-sm hover:bg-accent",
        selected && "bg-accent",
        focused && "bg-accent ring-1 ring-ring",
      )}
    >
      <span className="font-medium">
        {catalogueMissing ? (
          <>
            <IpHostname ip={destination.destination_ip} />
            <span className="ml-2 text-xs text-muted-foreground">— no metadata</span>
          </>
        ) : (
          destination.display_name
        )}
      </span>
      {!catalogueMissing && (
        <span className="text-xs text-muted-foreground">
          <IpHostname ip={destination.destination_ip} />
          {destination.city ? ` · ${destination.city}` : ""}
          {destination.country_code ? ` · ${destination.country_code}` : ""}
          {destination.asn != null ? ` · AS${destination.asn}` : ""}
        </span>
      )}
    </button>
  );
}

// ---------------------------------------------------------------------------
// utilities
// ---------------------------------------------------------------------------

function useDebounced<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const handle = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(handle);
  }, [value, delayMs]);
  return debounced;
}
