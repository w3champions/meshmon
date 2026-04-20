import { type CatalogueListQuery, useCatalogueListInfinite } from "@/api/hooks/catalogue";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { Bbox } from "@/lib/geo";
import { EntryCard } from "./EntryCard";

export interface CatalogueClusterDialogProps {
  open: boolean;
  onOpenChange(open: boolean): void;
  /**
   * Bucket bbox the operator clicked on the map. `null` while the dialog
   * is closed / no cluster is selected — the dialog guards its query
   * behind `cell !== null && open` so a stale bbox doesn't refire a
   * fetch on the next mount.
   */
  cell: Bbox | null;
  /**
   * Outer filters applied to the catalogue list — identical in shape to
   * the table's query, minus `bbox` (owned by the dialog). Shape, ASN,
   * network, country, IP prefix, and name filters all flow through.
   */
  filters: CatalogueListQuery;
  onOpenEntry(id: string): void;
}

/** Dialog page size — clamped by the backend to `1..=500`. */
const DIALOG_PAGE_SIZE = 50;

/**
 * Modal shown when the operator clicks a server-aggregated cluster pin
 * on the catalogue map. Owns its own `useCatalogueListInfinite` query
 * scoped to the cluster's bbox + any outer filters so the list stays
 * consistent with the current table view. Rows stream in pages of
 * {@link DIALOG_PAGE_SIZE}; the Load-more button pulls the next cursor.
 */
export function CatalogueClusterDialog({
  open,
  onOpenChange,
  cell,
  filters,
  onOpenEntry,
}: CatalogueClusterDialogProps) {
  // The generated `bbox` type is `number[]`; openapi-fetch serialises it
  // into the 4-element comma-separated query string the server expects
  // (`minLat,minLon,maxLat,maxLon`) via the operation's `style=form,
  // explode=false` spec — we pass the tuple through untouched.
  const bbox = cell ?? undefined;
  const infinite = useCatalogueListInfinite(
    { ...filters, bbox },
    { pageSize: DIALOG_PAGE_SIZE, enabled: cell !== null && open },
  );

  const rows = infinite.data?.pages.flatMap((p) => p.entries) ?? [];
  const total = infinite.data?.pages[0]?.total ?? 0;
  const { hasNextPage, isFetchingNextPage, fetchNextPage, isError } = infinite;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[70vh] flex-col gap-4 sm:max-w-md">
        <DialogHeader>
          <DialogTitle>
            Showing {rows.length} of {total} in this area
          </DialogTitle>
          <DialogDescription>Select a pin to open its details.</DialogDescription>
        </DialogHeader>

        {isError ? (
          <div role="alert" className="text-sm text-destructive">
            Failed to load cluster contents.
          </div>
        ) : null}

        <ul className="flex flex-col divide-y divide-border overflow-y-auto">
          {rows.map((entry) => (
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

        <div className="flex justify-end pt-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={!hasNextPage || isFetchingNextPage}
            onClick={() => fetchNextPage()}
          >
            {isFetchingNextPage ? "Loading…" : hasNextPage ? "Load more" : "All loaded"}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
