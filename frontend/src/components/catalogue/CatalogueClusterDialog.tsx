import type { CatalogueEntry } from "@/api/hooks/catalogue";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EntryCard } from "./EntryCard";

export interface CatalogueClusterDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
  entries: CatalogueEntry[];
  onOpenEntry(id: string): void;
}

/**
 * Modal shown when the operator clicks a marker cluster. Lists every pin in
 * the cluster with the shared `EntryCard` info block; clicking a row closes
 * the dialog and opens that entry's details drawer via `onOpenEntry`.
 */
export function CatalogueClusterDialog({
  open,
  onOpenChange,
  entries,
  onOpenEntry,
}: CatalogueClusterDialogProps) {
  const count = entries.length;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[70vh] flex-col gap-4 sm:max-w-md">
        <DialogHeader>
          <DialogTitle>
            {count} {count === 1 ? "pin" : "pins"} in this area
          </DialogTitle>
          <DialogDescription>Select a pin to open its details.</DialogDescription>
        </DialogHeader>
        <ul className="flex flex-col divide-y divide-border overflow-y-auto">
          {entries.map((entry) => (
            <li key={entry.id}>
              <button
                type="button"
                className="w-full cursor-pointer rounded-sm px-1 py-2 text-left transition-colors hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none"
                aria-label={`Open details for ${entry.display_name ?? entry.ip}`}
                onClick={() => {
                  onOpenEntry(entry.id);
                  onOpenChange(false);
                }}
              >
                <EntryCard entry={entry} />
              </button>
            </li>
          ))}
        </ul>
      </DialogContent>
    </Dialog>
  );
}
