import { useMemo } from "react";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { DrawMap } from "@/components/map/DrawMap";
import type { GeoShape } from "@/lib/geo";
import { EntryCard } from "./EntryCard";

export interface CatalogueMapProps {
  entries: CatalogueEntry[];
  shapes: GeoShape[];
  onShapesChange(next: GeoShape[]): void;
  onRowClick(id: string): void;
  className?: string;
}

interface EntryPopupProps {
  entry: CatalogueEntry;
  onOpen(): void;
}

export function EntryPopup({ entry, onOpen }: EntryPopupProps) {
  return (
    <div className="flex w-64 flex-col gap-2 text-sm">
      <EntryCard entry={entry} />
      <button
        type="button"
        className="self-start text-xs underline underline-offset-2 hover:text-foreground"
        aria-label={`Open details for ${entry.ip}`}
        onClick={onOpen}
      >
        Open details
      </button>
    </div>
  );
}

export function CatalogueMap({
  entries,
  shapes,
  onShapesChange,
  onRowClick,
  className,
}: CatalogueMapProps) {
  const pins = useMemo(
    () =>
      entries
        .filter((e) => e.latitude != null && e.longitude != null)
        .map((e) => ({
          id: e.id,
          lat: e.latitude as number,
          lon: e.longitude as number,
          popup: <EntryPopup entry={e} onOpen={() => onRowClick(e.id)} />,
        })),
    [entries, onRowClick],
  );

  return (
    <DrawMap shapes={shapes} onShapesChange={onShapesChange} pins={pins} className={className} />
  );
}
