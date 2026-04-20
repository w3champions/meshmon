import { Building2, Globe, MapPin, StickyNote } from "lucide-react";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { lookupCountryName } from "@/lib/countries";
import { cn } from "@/lib/utils";
import { formatWebsiteHost } from "./CatalogueTable";
import { StatusChip } from "./StatusChip";

interface EntryCardHeaderProps {
  entry: CatalogueEntry;
}

function EntryCardHeader({ entry }: EntryCardHeaderProps) {
  const hasDisplayName = !!entry.display_name;
  return (
    <div className="flex items-start justify-between gap-2">
      <div className="min-w-0 flex-1">
        <p
          className={cn(
            "truncate font-semibold text-foreground",
            hasDisplayName ? "text-sm" : "font-mono text-sm",
          )}
          title={hasDisplayName ? (entry.display_name ?? undefined) : entry.ip}
        >
          {entry.display_name ?? entry.ip}
        </p>
        {hasDisplayName ? (
          <p className="truncate font-mono text-xs text-muted-foreground" title={entry.ip}>
            {entry.ip}
          </p>
        ) : null}
      </div>
      <StatusChip status={entry.enrichment_status} />
    </div>
  );
}

interface EntryMetaRowProps {
  icon: React.ReactNode;
  label: string;
  children: React.ReactNode;
}

function EntryMetaRow({ icon, label, children }: EntryMetaRowProps) {
  return (
    <div className="flex items-start gap-2 text-xs">
      <span className="mt-0.5 flex-shrink-0 text-muted-foreground" aria-hidden="true">
        {icon}
      </span>
      <span className="sr-only">{label}</span>
      <span className="min-w-0 flex-1 text-foreground">{children}</span>
    </div>
  );
}

function formatLocation(entry: CatalogueEntry): string | null {
  const country = entry.country_name ?? lookupCountryName(entry.country_code);
  const city = entry.city ?? null;
  if (city && country) return `${city}, ${country}`;
  if (country) return country;
  if (city) return city;
  return null;
}

function formatNetwork(entry: CatalogueEntry): string | null {
  const asnPart = entry.asn != null ? `AS${entry.asn}` : null;
  const opPart = entry.network_operator ?? null;
  if (asnPart && opPart) return `${asnPart} · ${opPart}`;
  if (asnPart) return asnPart;
  if (opPart) return opPart;
  return null;
}

function firstLine(text: string): string {
  return text.split("\n")[0] ?? text;
}

interface EntryMetaProps {
  entry: CatalogueEntry;
}

export function EntryMeta({ entry }: EntryMetaProps) {
  const location = formatLocation(entry);
  const network = formatNetwork(entry);
  const website = entry.website ?? null;
  const notes = entry.notes ?? null;

  const websiteHref = website
    ? /^https?:\/\//i.test(website)
      ? website
      : `https://${website}`
    : null;
  const websiteHost = website ? formatWebsiteHost(website) : null;

  return (
    <div className="flex flex-col gap-1.5">
      {location ? (
        <EntryMetaRow icon={<MapPin className="h-3.5 w-3.5" />} label="Location">
          <span className="truncate" title={location}>
            {location}
          </span>
        </EntryMetaRow>
      ) : null}
      {network ? (
        <EntryMetaRow icon={<Building2 className="h-3.5 w-3.5" />} label="Network">
          <span className="truncate" title={network}>
            {network}
          </span>
        </EntryMetaRow>
      ) : null}
      {websiteHref && websiteHost ? (
        <EntryMetaRow icon={<Globe className="h-3.5 w-3.5" />} label="Website">
          <a
            href={websiteHref}
            target="_blank"
            rel="noopener noreferrer"
            onClick={(e) => e.stopPropagation()}
            className="block truncate text-primary underline-offset-2 hover:underline"
            title={website ?? undefined}
          >
            {websiteHost}
          </a>
        </EntryMetaRow>
      ) : null}
      {notes ? (
        <EntryMetaRow icon={<StickyNote className="h-3.5 w-3.5" />} label="Notes">
          <span className="block line-clamp-1 text-muted-foreground" title={notes}>
            {firstLine(notes)}
          </span>
        </EntryMetaRow>
      ) : null}
    </div>
  );
}

export interface EntryCardProps {
  entry: CatalogueEntry;
  className?: string;
}

/**
 * Shared rich info block used by the map pin popup and the cluster-dialog
 * list items. Keeps popup and list rows visually identical.
 */
export function EntryCard({ entry, className }: EntryCardProps) {
  return (
    <div className={cn("flex flex-col gap-2", className)}>
      <EntryCardHeader entry={entry} />
      <EntryMeta entry={entry} />
    </div>
  );
}
