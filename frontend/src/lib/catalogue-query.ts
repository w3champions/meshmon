import type { CatalogueListQuery } from "@/api/hooks/catalogue";
import type { FilterValue } from "@/components/filter/FilterRail";
import { shapesToPolygons } from "@/lib/geo";
import { normalizeIpPrefix } from "@/lib/ip-prefix";

/**
 * Map a `FilterRail` value to a `CatalogueListQuery`. Shared by
 * `DestinationPanel` and `CampaignComposer` so the two size-preview
 * sources stay aligned — diverging projections would produce
 * panel-vs-composer totals that disagree in the same viewport.
 *
 * IP prefixes are CIDR-normalised; shape filters are serialised as the
 * GeoJSON ring array the backend's `catalogue/shapes.rs` parser expects.
 */
export function destinationFilterToQuery(filter: FilterValue): CatalogueListQuery {
  const q: CatalogueListQuery = {};
  if (filter.countryCodes.length > 0) q.country_code = filter.countryCodes;
  if (filter.asns.length > 0) q.asn = filter.asns;
  if (filter.networks.length > 0) q.network = filter.networks;
  if (filter.cities.length > 0) q.city = filter.cities;
  if (filter.ipPrefix) {
    const normalized = normalizeIpPrefix(filter.ipPrefix);
    if (normalized) q.ip_prefix = normalized;
  }
  if (filter.nameSearch) q.name = filter.nameSearch;
  if (filter.shapes.length > 0) {
    q.shapes = JSON.stringify(shapesToPolygons(filter.shapes));
  }
  return q;
}
