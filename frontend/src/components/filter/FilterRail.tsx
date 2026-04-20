import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { components } from "@/api/schema.gen";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { GeoShape } from "@/lib/geo";
import { cn } from "@/lib/utils";

type FacetsResponse = components["schemas"]["FacetsResponse"];
type CountryFacet = components["schemas"]["CountryFacet"];
type AsnFacet = components["schemas"]["AsnFacet"];
type NetworkFacet = components["schemas"]["NetworkFacet"];
type CityFacet = components["schemas"]["CityFacet"];

/**
 * Client-side cap on how many facet options each group shows at once.
 * Applied AFTER the per-group search filter so typing can still reach
 * buckets beyond the initial cap.
 */
const TOP_N = 50;

const mapCountryFacet = (o: CountryFacet): FacetOption<string> => ({
  id: o.code,
  label: o.name ? `${o.name} (${o.code})` : o.code,
  searchText: `${o.code} ${o.name ?? ""}`,
  count: o.count,
});

const mapAsnFacet = (o: AsnFacet): FacetOption<number> => ({
  id: o.asn,
  label: `AS${o.asn}`,
  searchText: String(o.asn),
  count: o.count,
});

const mapNetworkFacet = (o: NetworkFacet): FacetOption<string> => ({
  id: o.name,
  label: o.name,
  searchText: o.name,
  count: o.count,
});

const mapCityFacet = (o: CityFacet): FacetOption<string> => ({
  id: o.name,
  label: o.name,
  searchText: o.name,
  count: o.count,
});

export interface FilterValue {
  countryCodes: string[];
  asns: number[];
  networks: string[];
  cities: string[];
  ipPrefix?: string;
  nameSearch?: string;
  shapes: GeoShape[];
}

export interface FilterRailProps {
  value: FilterValue;
  onChange(next: FilterValue): void;
  facets: FacetsResponse | undefined;
  onOpenMap?(): void;
}

export function FilterRail({ value, onChange, facets, onOpenMap }: FilterRailProps) {
  const hasFacets = facets !== undefined;

  return (
    <aside
      aria-label="Catalogue filters"
      className="flex w-full flex-col gap-2 rounded-md border bg-background p-3 text-sm"
    >
      <FacetGroup<CountryFacet, string>
        title="Country"
        options={facets?.countries ?? []}
        selected={value.countryCodes}
        hasFacets={hasFacets}
        searchPlaceholder="Search countries"
        clearLabel="Clear country filter"
        idToKey={(id) => id}
        mapOption={mapCountryFacet}
        onToggle={(code) =>
          onChange({ ...value, countryCodes: toggleInArray(value.countryCodes, code) })
        }
        onClear={() => onChange({ ...value, countryCodes: [] })}
      />

      <FacetGroup<AsnFacet, number>
        title="ASN"
        options={facets?.asns ?? []}
        selected={value.asns}
        hasFacets={hasFacets}
        searchPlaceholder="Search ASN"
        clearLabel="Clear ASN filter"
        idToKey={(id) => String(id)}
        mapOption={mapAsnFacet}
        onToggle={(asn) => onChange({ ...value, asns: toggleInArray(value.asns, asn) })}
        onClear={() => onChange({ ...value, asns: [] })}
      />

      <FacetGroup<NetworkFacet, string>
        title="Network"
        options={facets?.networks ?? []}
        selected={value.networks}
        hasFacets={hasFacets}
        searchPlaceholder="Search networks"
        clearLabel="Clear network filter"
        idToKey={(id) => id}
        mapOption={mapNetworkFacet}
        onToggle={(name) => onChange({ ...value, networks: toggleInArray(value.networks, name) })}
        onClear={() => onChange({ ...value, networks: [] })}
      />

      <FacetGroup<CityFacet, string>
        title="City"
        options={facets?.cities ?? []}
        selected={value.cities}
        hasFacets={hasFacets}
        searchPlaceholder="Search cities"
        clearLabel="Clear city filter"
        idToKey={(id) => id}
        mapOption={mapCityFacet}
        onToggle={(name) => onChange({ ...value, cities: toggleInArray(value.cities, name) })}
        onClear={() => onChange({ ...value, cities: [] })}
      />

      <FreeTextGroup
        title="Name"
        placeholder="Search display name"
        value={value.nameSearch ?? ""}
        onCommit={(next) => onChange({ ...value, nameSearch: next })}
      />

      <FreeTextGroup
        title="IP prefix"
        placeholder="e.g. 10.0.0. or 10.0.0.0/24"
        value={value.ipPrefix ?? ""}
        onCommit={(next) => onChange({ ...value, ipPrefix: next })}
      />

      <ShapeGroup
        shapes={value.shapes}
        onOpenMap={onOpenMap}
        onClear={() => onChange({ ...value, shapes: [] })}
      />
    </aside>
  );
}

function toggleInArray<T>(xs: readonly T[], item: T): T[] {
  return xs.includes(item) ? xs.filter((x) => x !== item) : [...xs, item];
}

interface GroupShellProps {
  title: string;
  summary?: string;
  badgeCount?: number;
  defaultOpen?: boolean;
  children: React.ReactNode;
  rightSlot?: React.ReactNode;
}

function GroupShell({
  title,
  summary,
  badgeCount,
  defaultOpen,
  children,
  rightSlot,
}: GroupShellProps) {
  // `<details>` is treated as uncontrolled: `defaultOpen` only sets the initial
  // state on mount. After mount, the user is in full control — later prop
  // changes MUST NOT retake control and re-open or re-close the group.
  const detailsRef = useRef<HTMLDetailsElement>(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: intentionally runs once on mount; re-running on defaultOpen would retake control from the user
  useEffect(() => {
    if (detailsRef.current && defaultOpen) {
      detailsRef.current.open = true;
    }
  }, []);
  return (
    <details
      ref={detailsRef}
      className="group rounded-md border border-transparent open:border-border open:bg-muted/30"
    >
      <summary className="flex cursor-pointer list-none items-center justify-between gap-2 rounded-md px-2 py-1.5 hover:bg-muted">
        <span className="flex items-center gap-2">
          <span className="font-medium">{title}</span>
          {badgeCount !== undefined && badgeCount > 0 && (
            <Badge variant="secondary" className="px-1.5 py-0">
              {badgeCount}
            </Badge>
          )}
          {summary !== undefined && (
            <span className="text-xs text-muted-foreground">{summary}</span>
          )}
        </span>
        {rightSlot}
      </summary>
      <div className="px-2 pb-3 pt-1">{children}</div>
    </details>
  );
}

interface FacetOption<K> {
  id: K;
  label: string;
  searchText: string;
  count: number;
}

interface FacetGroupProps<Facet, K> {
  title: string;
  options: Facet[];
  selected: readonly K[];
  hasFacets: boolean;
  searchPlaceholder: string;
  clearLabel: string;
  idToKey(id: K): string;
  mapOption(facet: Facet): FacetOption<K>;
  onToggle(id: K): void;
  onClear(): void;
}

function FacetGroup<Facet, K>({
  title,
  options,
  selected,
  hasFacets,
  searchPlaceholder,
  clearLabel,
  idToKey,
  mapOption,
  onToggle,
  onClear,
}: FacetGroupProps<Facet, K>) {
  const mapped = useMemo(() => options.map(mapOption), [options, mapOption]);

  return (
    <GroupShell
      title={title}
      badgeCount={selected.length}
      rightSlot={
        selected.length > 0 ? <ClearInlineButton onClear={onClear} label={clearLabel} /> : undefined
      }
    >
      <FacetList<K>
        options={mapped}
        selected={selected}
        onToggle={onToggle}
        hasFacets={hasFacets}
        searchPlaceholder={searchPlaceholder}
        idToKey={idToKey}
      />
    </GroupShell>
  );
}

interface FacetListProps<K> {
  options: FacetOption<K>[];
  selected: readonly K[];
  onToggle(id: K): void;
  hasFacets: boolean;
  searchPlaceholder: string;
  idToKey(id: K): string;
}

function FacetList<K>({
  options,
  selected,
  onToggle,
  hasFacets,
  searchPlaceholder,
  idToKey,
}: FacetListProps<K>) {
  const [query, setQuery] = useState("");
  const searchId = useId();

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (q.length === 0) return options;
    return options.filter((opt) => opt.searchText.toLowerCase().includes(q));
  }, [options, query]);

  // Apply the Top-N cap AFTER filtering so search can reach items past the
  // initial cap.
  const visible = filtered.slice(0, TOP_N);
  const truncated = filtered.length > TOP_N;

  if (!hasFacets) {
    return (
      <p className="px-1 text-xs text-muted-foreground" data-testid="facets-empty">
        Facets unavailable — filter options will appear once data loads.
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <Input
        id={searchId}
        type="search"
        value={query}
        placeholder={searchPlaceholder}
        aria-label={searchPlaceholder}
        onChange={(e) => setQuery(e.target.value)}
        className="h-8"
      />
      {options.length === 0 ? (
        <p className="px-1 text-xs text-muted-foreground">No options available.</p>
      ) : (
        <ul className="flex max-h-56 flex-col gap-0.5 overflow-y-auto">
          {visible.map((opt) => {
            const isSelected = selected.includes(opt.id);
            return (
              <li key={idToKey(opt.id)}>
                <button
                  type="button"
                  aria-pressed={isSelected}
                  onClick={() => onToggle(opt.id)}
                  className={cn(
                    "flex w-full items-center justify-between gap-2 rounded px-2 py-1 text-left text-xs hover:bg-muted",
                    isSelected && "bg-primary/10 font-medium text-foreground",
                  )}
                >
                  <span className="truncate">{opt.label}</span>
                  <span className="shrink-0 text-[10px] tabular-nums text-muted-foreground">
                    {opt.count}
                  </span>
                </button>
              </li>
            );
          })}
          {visible.length === 0 && (
            <li className="px-2 py-1 text-xs text-muted-foreground">No matches.</li>
          )}
        </ul>
      )}
      {truncated && (
        <p className="px-1 text-[11px] text-muted-foreground">
          Showing top {TOP_N} of {filtered.length}. Refine the search to narrow.
        </p>
      )}
    </div>
  );
}

interface FreeTextGroupProps {
  title: string;
  placeholder: string;
  value: string;
  onCommit(next: string | undefined): void;
}

function FreeTextGroup({ title, placeholder, value, onCommit }: FreeTextGroupProps) {
  return (
    <GroupShell title={title} summary={value.length > 0 ? value : undefined} defaultOpen={false}>
      <div className="flex flex-col gap-1">
        <Input
          type="search"
          value={value}
          placeholder={placeholder}
          aria-label={title}
          onChange={(e) => {
            const trimmed = e.target.value.trim();
            onCommit(trimmed.length === 0 ? undefined : e.target.value);
          }}
          className="h-8"
        />
      </div>
    </GroupShell>
  );
}

interface ShapeGroupProps {
  shapes: GeoShape[];
  onOpenMap?: () => void;
  onClear(): void;
}

function ShapeGroup({ shapes, onOpenMap, onClear }: ShapeGroupProps) {
  const hasShapes = shapes.length > 0;
  const buttonLabel = hasShapes ? "Edit map" : "Open map";
  const summary = hasShapes ? summarizeShapes(shapes) : "No shapes drawn";

  return (
    <GroupShell
      title="Map shapes"
      summary={summary}
      badgeCount={shapes.length}
      rightSlot={
        hasShapes ? <ClearInlineButton onClear={onClear} label="Clear map shapes" /> : undefined
      }
      defaultOpen={hasShapes}
    >
      <div className="flex items-center gap-2">
        <Button
          type="button"
          size="sm"
          variant={hasShapes ? "secondary" : "outline"}
          onClick={() => onOpenMap?.()}
          disabled={onOpenMap === undefined}
        >
          {buttonLabel}
        </Button>
        {hasShapes && (
          <Button type="button" size="sm" variant="ghost" onClick={onClear}>
            Clear
          </Button>
        )}
      </div>
    </GroupShell>
  );
}

function summarizeShapes(shapes: GeoShape[]): string {
  const counts = { polygon: 0, rectangle: 0, circle: 0 };
  for (const s of shapes) counts[s.kind] += 1;
  const parts: string[] = [];
  if (counts.polygon > 0) parts.push(`${counts.polygon} polygon${counts.polygon > 1 ? "s" : ""}`);
  if (counts.rectangle > 0)
    parts.push(`${counts.rectangle} rectangle${counts.rectangle > 1 ? "s" : ""}`);
  if (counts.circle > 0) parts.push(`${counts.circle} circle${counts.circle > 1 ? "s" : ""}`);
  return parts.join(", ");
}

interface ClearInlineButtonProps {
  onClear(): void;
  label: string;
}

function ClearInlineButton({ onClear, label }: ClearInlineButtonProps) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        onClear();
      }}
      className="rounded px-1.5 py-0.5 text-[11px] uppercase tracking-wide text-muted-foreground hover:bg-muted hover:text-foreground"
    >
      Clear
    </button>
  );
}
