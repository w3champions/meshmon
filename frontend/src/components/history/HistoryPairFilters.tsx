import { useEffect, useMemo, useState } from "react";
import type { ProbeProtocol } from "@/api/hooks/campaigns";
import {
  type HistoryDestination,
  type HistorySource,
  useHistoryDestinations,
  useHistorySources,
} from "@/api/hooks/history";
import { CustomRangeInputs } from "@/components/CustomRangeInputs";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { cn } from "@/lib/utils";

export type HistoryRange = "24h" | "7d" | "30d" | "90d" | "custom";

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
            const range = next as HistoryRange;
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
          onChange={({ from, to }) =>
            onChange({
              ...value,
              range: "custom",
              from: from || undefined,
              to: to || undefined,
            })
          }
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

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return sources;
    return sources.filter(
      (s) =>
        s.source_agent_id.toLowerCase().includes(q) || s.display_name.toLowerCase().includes(q),
    );
  }, [sources, query]);

  const label = selected ? selected.display_name : fallbackId ? fallbackId : "Pick a source";

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
            onChange={(e) => setQuery(e.target.value)}
            aria-label="Filter sources"
            className="mb-2"
            autoFocus
          />
          {loading ? (
            <p className="px-2 py-1 text-sm text-muted-foreground">Loading…</p>
          ) : filtered.length === 0 ? (
            <p className="px-2 py-1 text-sm text-muted-foreground">No sources match.</p>
          ) : (
            <div role="listbox" aria-label="Sources" className="max-h-72 overflow-y-auto">
              {filtered.map((s) => {
                const isSel = s.source_agent_id === selected?.source_agent_id;
                return (
                  <button
                    key={s.source_agent_id}
                    type="button"
                    role="option"
                    aria-selected={isSel}
                    onClick={() => {
                      onSelect(s.source_agent_id);
                      setOpen(false);
                      setQuery("");
                    }}
                    className={cn(
                      "flex w-full flex-col items-start gap-0.5 rounded px-2 py-1 text-left text-sm hover:bg-accent",
                      isSel && "bg-accent",
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

  // Debounce the query so we don't thrash the backend as the operator types.
  // Destination fetch is gated on source + includes the `q` param so the
  // backend can narrow on catalogue + IP substring.
  const debouncedQuery = useDebounced(query, 200);
  const destinations = useHistoryDestinations(source, debouncedQuery.trim() || undefined);

  const selected = useMemo(
    () => (destinations.data ?? []).find((d) => d.destination_ip === selectedId),
    [destinations.data, selectedId],
  );

  const disabled = !source;

  const triggerLabel = selected
    ? selected.display_name
    : selectedId
      ? selectedId
      : disabled
        ? "Pick a source first"
        : "Pick a destination";

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
            onChange={(e) => setQuery(e.target.value)}
            aria-label="Filter destinations"
            className="mb-2"
            autoFocus
          />
          {destinations.isPending ? (
            <p className="px-2 py-1 text-sm text-muted-foreground">Loading…</p>
          ) : destinations.isError ? (
            <p className="px-2 py-1 text-sm text-destructive">Failed to load destinations.</p>
          ) : (destinations.data ?? []).length === 0 ? (
            <p className="px-2 py-1 text-sm text-muted-foreground">No destinations match.</p>
          ) : (
            <div role="listbox" aria-label="Destinations" className="max-h-72 overflow-y-auto">
              {(destinations.data ?? []).map((d) => (
                <DestinationOption
                  key={d.destination_ip}
                  destination={d}
                  selected={d.destination_ip === selectedId}
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
  onClick(): void;
}

function DestinationOption({ destination, selected, onClick }: DestinationOptionProps) {
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
      type="button"
      role="option"
      aria-selected={selected}
      onClick={onClick}
      className={cn(
        "flex w-full flex-col items-start gap-0.5 rounded px-2 py-1 text-left text-sm hover:bg-accent",
        selected && "bg-accent",
      )}
    >
      <span className="font-medium">
        {catalogueMissing ? (
          <>
            <span className="font-mono">{destination.destination_ip}</span>
            <span className="ml-2 text-xs text-muted-foreground">— no metadata</span>
          </>
        ) : (
          destination.display_name
        )}
      </span>
      {!catalogueMissing && (
        <span className="font-mono text-xs text-muted-foreground">
          {destination.destination_ip}
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
