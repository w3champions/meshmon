import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDownIcon, ChevronRightIcon } from "lucide-react";
import { useState } from "react";
import type {
  CatalogueEntry,
  CataloguePasteRequest,
  CataloguePasteResponse,
} from "@/api/hooks/catalogue";
import { catalogueEntryKey, usePasteCatalogue } from "@/api/hooks/catalogue";
import type { CountryValue } from "@/components/catalogue/CountryPicker";
import { CountryPicker } from "@/components/catalogue/CountryPicker";
import { StatusChip } from "@/components/catalogue/StatusChip";
import type { LocationPickerValue } from "@/components/map/LocationPicker";
import { LocationPicker } from "@/components/map/LocationPicker";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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

type PasteMetadataBody = NonNullable<CataloguePasteRequest["metadata"]>;

interface MetadataDraft {
  displayName: string;
  city: string;
  country: CountryValue | null;
  location: LocationPickerValue | null;
  website: string;
  notes: string;
}

const EMPTY_METADATA: MetadataDraft = {
  displayName: "",
  city: "",
  country: null,
  location: null,
  website: "",
  notes: "",
};

/**
 * Build the wire-shape metadata body from the panel draft. Blank text
 * fields and nulled pickers collapse to an omitted key; when nothing
 * is set the function returns `undefined` so the caller can leave
 * `metadata` off the paste body entirely (pre-T52 contract).
 */
function toMetadataWire(d: MetadataDraft): PasteMetadataBody | undefined {
  const body: PasteMetadataBody = {};
  if (d.displayName.trim()) body.display_name = d.displayName.trim();
  if (d.city.trim()) body.city = d.city.trim();
  if (d.country) {
    body.country_code = d.country.code;
    body.country_name = d.country.name;
  }
  if (d.location) {
    body.latitude = d.location.latitude;
    body.longitude = d.location.longitude;
  }
  if (d.website.trim()) body.website = d.website.trim();
  if (d.notes.trim()) body.notes = d.notes.trim();
  return Object.keys(body).length > 0 ? body : undefined;
}

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

/**
 * Read-only renderer for the Display name column in the post-paste
 * staging view. Sources the name from the per-entry query cache so
 * the value reflects whatever the paste wrote (per-IP override, panel
 * default, or nothing) without the staging component needing to
 * thread each value through local state. Falls back to the panel
 * default so the operator sees the value they asked for even when the
 * paste's merge refused the override on a locked existing row.
 */
function StagingDisplayName({ id, fallback }: { id: string; fallback: string }) {
  const { data } = useQuery<CatalogueEntry>({
    queryKey: catalogueEntryKey(id),
    enabled: false,
    initialData: undefined,
  });
  const name = data?.display_name ?? (fallback.trim() || undefined);
  return name ? (
    <span className="text-sm">{name}</span>
  ) : (
    <span className="text-xs text-muted-foreground">—</span>
  );
}

interface MetadataPanelProps {
  value: MetadataDraft;
  onChange(next: MetadataDraft): void;
  open: boolean;
  onOpenChange(next: boolean): void;
}

/**
 * Collapsible "Default metadata (optional)" panel shown inside the
 * Add IPs dialog. Each filled field applies to every accepted IP via
 * `PasteRequest.metadata`; empty fields are omitted from the wire
 * body. Paired pickers (Country, Location) emit both halves together
 * so the server's paired-atomicity rule never rejects a half-filled
 * submission.
 */
function MetadataPanel({ value, onChange, open, onOpenChange }: MetadataPanelProps) {
  const panelId = "paste-metadata-panel";
  return (
    <div className="rounded-md border border-border">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => onOpenChange(!open)}
        className="w-full flex items-center gap-2 px-3 py-2 text-sm font-medium text-left hover:bg-muted/50"
      >
        {open ? (
          <ChevronDownIcon className="h-4 w-4" aria-hidden />
        ) : (
          <ChevronRightIcon className="h-4 w-4" aria-hidden />
        )}
        Default metadata (optional)
      </button>
      {open && (
        <div id={panelId} className="grid gap-3 border-t border-border p-3 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="paste-metadata-display-name">Display name</Label>
            <Input
              id="paste-metadata-display-name"
              value={value.displayName}
              onChange={(e) => onChange({ ...value, displayName: e.target.value })}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="paste-metadata-city">City</Label>
            <Input
              id="paste-metadata-city"
              value={value.city}
              onChange={(e) => onChange({ ...value, city: e.target.value })}
            />
          </div>
          <div className="space-y-1 sm:col-span-2">
            <Label htmlFor="paste-metadata-country">Country</Label>
            <CountryPicker
              id="paste-metadata-country"
              value={value.country}
              onChange={(next) => onChange({ ...value, country: next })}
            />
          </div>
          <div className="space-y-1 sm:col-span-2">
            <span className="text-sm font-medium">Location</span>
            <LocationPicker
              value={value.location}
              onChange={(next) => onChange({ ...value, location: next })}
              heightClassName="h-48"
            />
          </div>
          <div className="space-y-1 sm:col-span-2">
            <Label htmlFor="paste-metadata-website">Website</Label>
            <Input
              id="paste-metadata-website"
              value={value.website}
              onChange={(e) => onChange({ ...value, website: e.target.value })}
            />
          </div>
          <div className="space-y-1 sm:col-span-2">
            <Label htmlFor="paste-metadata-notes">Notes</Label>
            <textarea
              id="paste-metadata-notes"
              value={value.notes}
              onChange={(e) => onChange({ ...value, notes: e.target.value })}
              rows={2}
              className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            />
          </div>
        </div>
      )}
    </div>
  );
}

interface SkippedSummaryNoticeProps {
  summary: NonNullable<CataloguePasteResponse["skipped_summary"]>;
}

/**
 * Inline confirmation shown when the server refused one or more
 * metadata writes because the target fields were already operator-
 * locked on existing rows. The message is `role="status"` so it
 * auto-announces without stealing focus.
 */
function SkippedSummaryNotice({ summary }: SkippedSummaryNoticeProps) {
  const fields = Object.keys(summary.skipped_field_counts).sort();
  const fieldList = fields.length > 0 ? ` (${fields.join(", ")})` : "";
  const plural = summary.rows_with_skips === 1 ? "row" : "rows";
  return (
    <p role="status" aria-label="Metadata skip summary" className="text-sm text-muted-foreground">
      Applied defaults. {summary.rows_with_skips} existing {plural} kept their operator-locked
      values{fieldList}.
    </p>
  );
}

export function PasteStaging({ open, onOpenChange }: PasteStagingProps) {
  const [text, setText] = useState("");
  const [stagingRows, setStagingRows] = useState<StagingRow[]>([]);
  const [hasPosted, setHasPosted] = useState(false);
  const [metadata, setMetadata] = useState<MetadataDraft>(EMPTY_METADATA);
  const [metadataOpen, setMetadataOpen] = useState(false);
  // Per-IP display-name overrides keyed by the literal IP string —
  // same key shape the backend expects on the wire. An absent entry or
  // a blank value falls back to `metadata.displayName`. State lives
  // here (not per-row) so the operator can edit names before, during,
  // or after a paste without losing input on rerenders.
  const [perIpDisplayNames, setPerIpDisplayNames] = useState<Record<string, string>>({});
  const [skippedSummary, setSkippedSummary] = useState<
    CataloguePasteResponse["skipped_summary"] | null
  >(null);
  const queryClient = useQueryClient();
  const pasteMutation = usePasteCatalogue();

  const outcome: ParseOutcome =
    text.trim().length > 0 ? parsePasteInput(text) : { accepted: [], rejected: [] };

  /**
   * Filter the local override map down to IPs the operator actually
   * set a non-blank name for, then trim the values so server-side
   * whitespace-only overrides don't cause no-op locked writes.
   */
  const buildPerIpWire = (source: Record<string, string>): Record<string, string> => {
    const out: Record<string, string> = {};
    for (const [ip, raw] of Object.entries(source)) {
      const trimmed = raw.trim();
      if (trimmed) out[ip] = trimmed;
    }
    return out;
  };

  const handleAdd = async () => {
    if (outcome.accepted.length === 0) return;
    const ips = outcome.accepted.map((a) => a.ip);
    const metadataBody = toMetadataWire(metadata);
    const perIp = buildPerIpWire(perIpDisplayNames);
    const body: CataloguePasteRequest = {
      ips,
      ...(metadataBody ? { metadata: metadataBody } : {}),
      ...(Object.keys(perIp).length > 0 ? { per_ip_display_names: perIp } : {}),
    };
    let result: CataloguePasteResponse;
    try {
      result = await pasteMutation.mutateAsync(body);
    } catch {
      // `pasteMutation.isError` drives the error banner. Clear stale rows from
      // a previous successful paste so the UI doesn't show a half-populated
      // staging table alongside the error.
      setStagingRows([]);
      setHasPosted(false);
      setSkippedSummary(null);
      return;
    }
    setSkippedSummary(result.skipped_summary ?? null);

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
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        // Wider layout makes room for the metadata panel + staging
        // table side-by-side content; `max-h-[90vh]` caps height so
        // the dialog fits laptop viewports; the middle region is the
        // only scroll host so the footer stays on-screen.
        className="w-[95vw] sm:max-w-3xl max-h-[90vh] !grid-rows-none !grid-cols-none !grid-flow-row !gap-0 flex flex-col p-0"
      >
        <DialogHeader className="px-6 pt-6 pb-2">
          <DialogTitle>Add IPs</DialogTitle>
          <DialogDescription>
            Paste one IP per line or comma-separated. Duplicate entries will be collapsed.
          </DialogDescription>
        </DialogHeader>

        <div className="flex-1 min-h-0 overflow-y-auto px-6 pb-4 flex flex-col gap-3">
          <div className="space-y-1.5">
            <Label htmlFor="paste-ip-textarea">IP addresses</Label>
            <textarea
              id="paste-ip-textarea"
              value={text}
              onChange={(e) => {
                setText(e.target.value);
                // Reset staging rows when the user edits after a POST.
                // Per-IP display names stay — matching IPs the operator
                // pastes again (typo fix, re-add) keep the name they
                // already typed. Stale names for IPs no longer in the
                // list are filtered out at submit time by
                // `buildPerIpWire`.
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

          {/* Fixed-height slot for rejected tokens — reserved even when empty so the
              table below doesn't jump as the user types. */}
          <div className="min-h-8">
            {outcome.rejected.length > 0 && (
              <ul aria-label="Invalid tokens" className="flex flex-wrap gap-1.5 list-none p-0">
                {outcome.rejected.map((r) => (
                  <li key={`${r.token}::${r.reason}`}>
                    <RejectedChip token={r.token} reason={r.reason} />
                  </li>
                ))}
              </ul>
            )}
          </div>

          <MetadataPanel
            value={metadata}
            onChange={setMetadata}
            open={metadataOpen}
            onOpenChange={setMetadataOpen}
          />

          {skippedSummary && skippedSummary.rows_with_skips > 0 && (
            <SkippedSummaryNotice summary={skippedSummary} />
          )}

          {!hasPosted && outcome.accepted.length > 0 && (
            <div className="border rounded-md">
              <Table aria-label="Parsed IPs">
                <TableHeader>
                  <TableRow>
                    <TableHead>IP address</TableHead>
                    <TableHead>Display name</TableHead>
                    <TableHead>Status</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {outcome.accepted.map((a) => (
                    <TableRow key={a.ip}>
                      <TableCell>
                        <span>{a.ip}</span>
                        {a.dupeCount > 1 && (
                          <span className="ml-2 text-xs text-muted-foreground">×{a.dupeCount}</span>
                        )}
                      </TableCell>
                      <TableCell>
                        <Input
                          aria-label={`Display name for ${a.ip}`}
                          placeholder={metadata.displayName || "Optional name"}
                          value={perIpDisplayNames[a.ip] ?? ""}
                          onChange={(e) =>
                            setPerIpDisplayNames((prev) => ({
                              ...prev,
                              [a.ip]: e.target.value,
                            }))
                          }
                          className="h-8"
                        />
                      </TableCell>
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
            <div className="border rounded-md">
              <Table aria-label="Staged IPs">
                <TableHeader>
                  <TableRow>
                    <TableHead>IP address</TableHead>
                    <TableHead>Display name</TableHead>
                    <TableHead>Status</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {stagingRows.map((row) => (
                    <TableRow key={row.ip}>
                      <TableCell>
                        <span>{row.ip}</span>
                        {row.dupeCount > 1 && (
                          <span className="ml-2 text-xs text-muted-foreground">
                            ×{row.dupeCount}
                          </span>
                        )}
                      </TableCell>
                      <TableCell>
                        <StagingDisplayName id={row.id} fallback={metadata.displayName} />
                      </TableCell>
                      <TableCell>
                        <StagingChip id={row.id} />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}

          {pasteMutation.isError && (
            <p role="alert" className="text-sm text-destructive">
              Failed to add entries. Please try again.
            </p>
          )}
        </div>

        <DialogFooter className="flex flex-row gap-2 border-t bg-background px-6 py-4">
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
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
