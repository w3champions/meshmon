import { useCallback, useMemo, useState } from "react";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { DrawMap } from "@/components/map/DrawMap";
import type { GeoShape } from "@/lib/geo";
import { CatalogueClusterDialog } from "./CatalogueClusterDialog";
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
  const [clusterPinIds, setClusterPinIds] = useState<string[] | null>(null);

  const entriesById = useMemo(() => {
    const map = new Map<string, CatalogueEntry>();
    for (const entry of entries) map.set(entry.id, entry);
    return map;
  }, [entries]);

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

  const handleClusterClick = useCallback((ids: string[]) => {
    setClusterPinIds(ids);
  }, []);

  const handleDialogOpenChange = useCallback((open: boolean) => {
    if (!open) setClusterPinIds(null);
  }, []);

  // Preserve the cluster's child-marker ordering while filtering out ids whose
  // entry has since vanished (e.g. background refetch trimmed the list).
  const clusterEntries = useMemo(() => {
    if (!clusterPinIds) return [];
    const out: CatalogueEntry[] = [];
    for (const id of clusterPinIds) {
      const entry = entriesById.get(id);
      if (entry) out.push(entry);
    }
    return out;
  }, [clusterPinIds, entriesById]);

  return (
    <>
      <DrawMap
        shapes={shapes}
        onShapesChange={onShapesChange}
        pins={pins}
        onClusterClick={handleClusterClick}
        className={className}
      />
      <CatalogueClusterDialog
        open={clusterPinIds !== null}
        onOpenChange={handleDialogOpenChange}
        entries={clusterEntries}
        onOpenEntry={onRowClick}
      />
    </>
  );
}
