import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import type { CatalogueEntry, CataloguePasteResponse } from "@/api/hooks/catalogue";
import { catalogueEntryKey, usePasteCatalogue } from "@/api/hooks/catalogue";
import { StatusChip } from "@/components/catalogue/StatusChip";
import { Badge } from "@/components/ui/badge";
import type { ParseOutcome } from "@/lib/catalogue-parse";
import { parsePasteInput } from "@/lib/catalogue-parse";

export interface PasteStagingProps {
  onClose(): void;
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

export function PasteStaging({ onClose }: PasteStagingProps) {
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
    const result: CataloguePasteResponse = await pasteMutation.mutateAsync({ ips });

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
    <section aria-label="Paste IPs staging panel">
      <div>
        <label htmlFor="paste-ip-textarea">Paste IPs</label>
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

      <div>
        <button
          type="button"
          onClick={() => void handleAdd()}
          disabled={!canAdd}
          aria-busy={pasteMutation.isPending}
        >
          {pasteMutation.isPending ? "Adding…" : "Add"}
        </button>
        <button type="button" onClick={onClose}>
          Close
        </button>
      </div>

      {pasteMutation.isError && <p role="alert">Failed to add entries. Please try again.</p>}

      {!hasPosted && outcome.accepted.length > 0 && (
        <table aria-label="Parsed IPs">
          <thead>
            <tr>
              <th>IP address</th>
              <th>Count</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {outcome.accepted.map((a) => (
              <tr key={a.ip}>
                <td>{a.ip}</td>
                <td>{a.dupeCount > 1 ? `×${a.dupeCount}` : null}</td>
                <td>
                  <StatusChip status="pending" />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {hasPosted && stagingRows.length > 0 && (
        <table aria-label="Staged IPs">
          <thead>
            <tr>
              <th>IP address</th>
              <th>Count</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {stagingRows.map((row) => (
              <tr key={row.ip}>
                <td>{row.ip}</td>
                <td>{row.dupeCount > 1 ? `×${row.dupeCount}` : null}</td>
                <td>
                  <StagingChip id={row.id} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
