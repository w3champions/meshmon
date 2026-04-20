import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import type { CatalogueEntry, CataloguePasteResponse } from "@/api/hooks/catalogue";
import { catalogueEntryKey, usePasteCatalogue } from "@/api/hooks/catalogue";
import { StatusChip } from "@/components/catalogue/StatusChip";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import type { ParseOutcome } from "@/lib/catalogue-parse";
import { parsePasteInput } from "@/lib/catalogue-parse";

export interface PasteStagingProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** Human-readable label for a parser rejection reason. */
function rejectionLabel(reason: string): string {
  if (reason.startsWith("cidr_not_allowed:")) {
    return "IP addresses only — CIDR ranges aren't allowed as catalogue entries";
  }
  return "Not a valid IP address";
}

interface RejectedChipProps {
  token: string;
  reason: string;
}

/**
 * Compact red pill for a rejected paste token. Mirrors `StatusChip`'s visual
 * vocabulary (rounded `Badge` with destructive tint) but is specific to
 * parse-time rejection — the full reason shows on hover via the `title` attr.
 */
function RejectedChip({ token, reason }: RejectedChipProps) {
  const label = rejectionLabel(reason);
  return (
    <Badge variant="destructive" title={label} aria-label={`${token}: ${label}`}>
      {token}
    </Badge>
  );
}

/** A single staged row after a successful POST — keyed by IP, carries the server-assigned id. */
interface StagingRow {
  ip: string;
  id: string;
  dupeCount: number;
}

/**
 * Chip for a staged row that observes the per-entry query cache.
 * When the SSE stream writes an enrichment_progress event the cache updates
 * and this component re-renders with the new status.
 */
function StagingChip({ id }: { id: string }) {
  const { data } = useQuery<CatalogueEntry>({
    queryKey: catalogueEntryKey(id),
    enabled: false,
    initialData: undefined,
  });
  const status = data?.enrichment_status ?? "pending";
  return <StatusChip status={status} />;
}

export function PasteStaging({ open, onOpenChange }: PasteStagingProps) {
  const [text, setText] = useState("");
  const [stagingRows, setStagingRows] = useState<StagingRow[]>([]);
  const [hasPosted, setHasPosted] = useState(false);
  const queryClient = useQueryClient();
  const pasteMutation = usePasteCatalogue();

  const outcome: ParseOutcome =
    text.trim().length > 0 ? parsePasteInput(text) : { accepted: [], rejected: [] };

  const handleAdd = async () => {
    if (outcome.accepted.length === 0) return;
    const ips = outcome.accepted.map((a) => a.ip);
    let result: CataloguePasteResponse;
    try {
      result = await pasteMutation.mutateAsync({ ips });
    } catch {
      // `pasteMutation.isError` drives the error banner. Clear stale rows from
      // a previous successful paste so the UI doesn't show a half-populated
      // staging table alongside the error.
      setStagingRows([]);
      setHasPosted(false);
      return;
    }

    // Build a map from ip → id from the response
    const ipToId = new Map<string, string>();
    for (const entry of result.created) {
      ipToId.set(entry.ip, entry.id);
    }
    for (const entry of result.existing) {
      ipToId.set(entry.ip, entry.id);
    }

    // Seed the query cache with the returned entries so the chip can read initial status
    for (const entry of [...result.created, ...result.existing]) {
      queryClient.setQueryData<CatalogueEntry>(catalogueEntryKey(entry.id), entry);
    }

    const rows: StagingRow[] = outcome.accepted
      .map((a) => ({
        ip: a.ip,
        id: ipToId.get(a.ip) ?? "",
        dupeCount: a.dupeCount,
      }))
      .filter((r) => r.id !== "");

    setStagingRows(rows);
    setHasPosted(true);
  };

  const canAdd = outcome.accepted.length > 0 && !pasteMutation.isPending;

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent side="right" className="w-full overflow-y-auto sm:max-w-xl">
        <SheetHeader>
          <SheetTitle>Add IPs</SheetTitle>
          <SheetDescription>
            Paste one IP per line or comma-separated. Duplicate entries will be collapsed.
          </SheetDescription>
        </SheetHeader>

        <div className="mt-4 space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="paste-ip-textarea">IP addresses</Label>
            <textarea
              id="paste-ip-textarea"
              value={text}
              onChange={(e) => {
                setText(e.target.value);
                // Reset staging rows when the user edits after a POST
                if (hasPosted) {
                  setStagingRows([]);
                  setHasPosted(false);
                }
              }}
              placeholder="One IP per line or comma-separated"
              rows={6}
              aria-label="Paste IPs"
              className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            />
          </div>

          {outcome.rejected.length > 0 && (
            <ul aria-label="Invalid tokens" className="flex flex-wrap gap-1.5 list-none p-0">
              {outcome.rejected.map((r) => (
                <li key={`${r.token}::${r.reason}`}>
                  <RejectedChip token={r.token} reason={r.reason} />
                </li>
              ))}
            </ul>
          )}

          {pasteMutation.isError && (
            <p role="alert" className="text-sm text-destructive">
              Failed to add entries. Please try again.
            </p>
          )}

          {!hasPosted && outcome.accepted.length > 0 && (
            <div>
              <p className="mb-2 text-sm font-medium text-muted-foreground">Parsed IPs</p>
              <Table aria-label="Parsed IPs">
                <TableHeader>
                  <TableRow>
                    <TableHead>IP address</TableHead>
                    <TableHead>Count</TableHead>
                    <TableHead>Status</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {outcome.accepted.map((a) => (
                    <TableRow key={a.ip}>
                      <TableCell>{a.ip}</TableCell>
                      <TableCell>{a.dupeCount > 1 ? `×${a.dupeCount}` : null}</TableCell>
                      <TableCell>
                        <StatusChip status="pending" />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}

          {hasPosted && stagingRows.length > 0 && (
            <div>
              <p className="mb-2 text-sm font-medium text-muted-foreground">Staged IPs</p>
              <Table aria-label="Staged IPs">
                <TableHeader>
                  <TableRow>
                    <TableHead>IP address</TableHead>
                    <TableHead>Count</TableHead>
                    <TableHead>Status</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {stagingRows.map((row) => (
                    <TableRow key={row.ip}>
                      <TableCell>{row.ip}</TableCell>
                      <TableCell>{row.dupeCount > 1 ? `×${row.dupeCount}` : null}</TableCell>
                      <TableCell>
                        <StagingChip id={row.id} />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </div>

        <SheetFooter className="mt-6 flex flex-row gap-2">
          <Button
            type="button"
            onClick={() => void handleAdd()}
            disabled={!canAdd}
            aria-busy={pasteMutation.isPending}
          >
            {pasteMutation.isPending ? "Adding…" : "Add"}
          </Button>
          <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
            Close
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
