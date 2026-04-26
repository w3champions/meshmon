/**
 * CandidateRef — IP-reference renderer for campaign candidate surfaces.
 *
 * Three display modes:
 * - `compact`: name + IP + city/ASN/operator chips + hostname + "Open" icon button.
 * - `header`: full enrichment card with display name (large), IP, hostname, geo, ASN,
 *             network operator, website link, notes, and open-details buttons.
 * - `inline`: display_name (or IP fallback) as a clickable text trigger.
 *
 * The "Open in catalogue" button uses the nearest `<CatalogueDrawerOverlay>`;
 * "Open agent detail" navigates to `/agents/:id` via TanStack Router.
 */

import { ExternalLink } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";
import { IpHostname } from "@/components/ip-hostname/IpHostname";
import { lookupCountryName } from "@/lib/countries";
import { useCatalogueDrawer } from "@/components/catalogue/CatalogueDrawerOverlay";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type CandidateRefData = {
  ip: string;
  display_name?: string | null;
  city?: string | null;
  country_code?: string | null;
  asn?: number | null;
  network_operator?: string | null;
  website?: string | null;
  notes?: string | null;
  hostname?: string | null;
  is_mesh_member: boolean;
  agent_id?: string | null;
};

export type CandidateRefProps = {
  mode: "compact" | "header" | "inline";
  data: CandidateRefData;
};

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function CandidateRef({ mode, data }: CandidateRefProps) {
  if (mode === "compact") return <CompactRef data={data} />;
  if (mode === "header") return <HeaderRef data={data} />;
  return <InlineRef data={data} />;
}

// ---------------------------------------------------------------------------
// Compact mode
// ---------------------------------------------------------------------------

function CompactRef({ data }: { data: CandidateRefData }) {
  const { open } = useCatalogueDrawer();

  return (
    <div className="candidate-ref candidate-ref-compact flex flex-wrap items-center gap-1 text-sm">
      <span className="candidate-name font-medium">{data.display_name ?? data.ip}</span>
      <span className="candidate-ip font-mono text-muted-foreground">
        <IpHostname ip={data.ip} />
      </span>
      {data.city && (
        <span className="chip inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-xs">
          {data.city}
        </span>
      )}
      {data.asn != null && (
        <span className="chip inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-xs">
          AS{data.asn}
        </span>
      )}
      {data.network_operator && (
        <span className="chip inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-xs">
          {data.network_operator}
        </span>
      )}
      <OpenAffordance ip={data.ip} kind="icon" onOpen={open} />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Header mode
// ---------------------------------------------------------------------------

function HeaderRef({ data }: { data: CandidateRefData }) {
  const { open } = useCatalogueDrawer();
  const navigate = useNavigate();

  const countryName = data.country_code ? lookupCountryName(data.country_code) : null;
  const countryLabel = [countryFlag(data.country_code), countryName ?? data.country_code]
    .filter(Boolean)
    .join(" ");

  const handleOpenAgent = () => {
    if (data.agent_id) {
      void navigate({ to: "/agents/$id", params: { id: data.agent_id } });
    }
  };

  return (
    <div className="candidate-ref candidate-ref-header space-y-2 p-3 rounded-md border bg-muted/20">
      {/* Name row */}
      <div className="flex items-start justify-between gap-2">
        <div>
          <div className="text-base font-semibold leading-tight">
            {data.display_name ?? data.ip}
          </div>
          <div className="font-mono text-sm text-muted-foreground mt-0.5">
            <IpHostname ip={data.ip} />
          </div>
        </div>

        {/* Action buttons */}
        <div className="flex gap-1 shrink-0">
          <button
            type="button"
            onClick={() => open(data.ip)}
            className="inline-flex items-center gap-1 rounded px-2 py-1 text-xs font-medium bg-secondary hover:bg-secondary/80 transition-colors"
            aria-label="Open in catalogue"
          >
            <ExternalLink className="h-3 w-3" aria-hidden="true" />
            Catalogue
          </button>
          {data.is_mesh_member && data.agent_id && (
            <button
              type="button"
              onClick={handleOpenAgent}
              className="inline-flex items-center gap-1 rounded px-2 py-1 text-xs font-medium bg-secondary hover:bg-secondary/80 transition-colors"
              aria-label="Open agent detail"
            >
              <ExternalLink className="h-3 w-3" aria-hidden="true" />
              Agent
            </button>
          )}
        </div>
      </div>

      {/* Geo + meta chips */}
      <div className="flex flex-wrap gap-1">
        {data.city && (
          <span className="chip inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-xs">
            {data.city}
          </span>
        )}
        {countryLabel && (
          <span className="chip inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-xs">
            {countryLabel}
          </span>
        )}
        {data.asn != null && (
          <span className="chip inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-xs">
            AS{data.asn}
          </span>
        )}
        {data.network_operator && (
          <span className="chip inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-xs">
            {data.network_operator}
          </span>
        )}
      </div>

      {/* Website */}
      {data.website && (
        <div className="text-xs">
          <a
            href={data.website}
            target="_blank"
            rel="noreferrer noopener"
            className="text-primary underline-offset-2 hover:underline inline-flex items-center gap-1"
          >
            {data.website}
            <ExternalLink className="h-3 w-3" aria-hidden="true" />
          </a>
        </div>
      )}

      {/* Notes (truncated) */}
      {data.notes && (
        <div className="text-xs text-muted-foreground line-clamp-2" title={data.notes}>
          {data.notes}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Inline mode
// ---------------------------------------------------------------------------

function InlineRef({ data }: { data: CandidateRefData }) {
  const { open } = useCatalogueDrawer();
  const label = data.display_name ?? data.ip;

  return (
    <button
      type="button"
      onClick={() => open(data.ip)}
      className="text-primary underline-offset-2 hover:underline text-sm"
    >
      {label}
    </button>
  );
}

// ---------------------------------------------------------------------------
// Shared open affordance
// ---------------------------------------------------------------------------

interface OpenAffordanceProps {
  ip: string;
  kind: "icon";
  onOpen: (ip: string) => void;
}

function OpenAffordance({ ip, onOpen }: OpenAffordanceProps) {
  return (
    <button
      type="button"
      onClick={() => onOpen(ip)}
      className="ml-1 inline-flex items-center justify-center rounded p-0.5 text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
      aria-label="Open in catalogue"
    >
      <ExternalLink className="h-3.5 w-3.5" aria-hidden="true" />
    </button>
  );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Convert an ISO 3166-1 alpha-2 country code to a flag emoji using Unicode
 * regional indicator symbols. Works in all modern browsers.
 */
function countryFlag(code: string | null | undefined): string | null {
  if (!code || code.length !== 2) return null;
  const upper = code.toUpperCase();
  // Regional Indicator Symbol Letter A starts at U+1F1E6.
  const flag = String.fromCodePoint(
    0x1f1e6 + upper.charCodeAt(0) - 65,
    0x1f1e6 + upper.charCodeAt(1) - 65,
  );
  return flag;
}
