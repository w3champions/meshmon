/**
 * Reusable sortable column header primitive.
 *
 * Generic over the sort-column type so multiple tables can share the same
 * three-state cycle (`none → asc → desc → none`) and aria-sort wiring.
 * Lifted out of `CatalogueTable.tsx` so the I5 paginated pair-details
 * dialog can reuse the same control. Behaviour and styling are unchanged
 * from the catalogue's original implementation.
 *
 * `aria-sort` lives on the enclosing element (each call site owns its own
 * `<th>` / `role="columnheader"` cell); this component only owns the
 * click-cycle button + focus ring.
 */

import { ChevronDown, ChevronsUpDown, ChevronUp } from "lucide-react";

export type SortDir = "asc" | "desc";

export interface SortState<TCol extends string> {
  col: TCol | null;
  dir: SortDir | null;
}

export interface SortIconProps {
  active: SortDir | null;
}

export function SortIcon({ active }: SortIconProps) {
  if (active === "asc") return <ChevronUp className="h-3.5 w-3.5" aria-hidden="true" />;
  if (active === "desc") return <ChevronDown className="h-3.5 w-3.5" aria-hidden="true" />;
  return <ChevronsUpDown className="h-3.5 w-3.5 opacity-50" aria-hidden="true" />;
}

/**
 * Returns the next sort tuple in the three-state cycle
 * `none → asc → desc → none` for the given column.
 */
export function nextSort<TCol extends string>(
  col: TCol,
  sort: SortState<TCol>,
): [TCol | null, SortDir | null] {
  const active = sort.col === col ? sort.dir : null;
  if (active === null) return [col, "asc"];
  if (active === "asc") return [col, "desc"];
  return [null, null];
}

export interface SortableHeaderProps<TCol extends string> {
  col: TCol;
  label: string;
  sort: SortState<TCol>;
  onSortChange: (col: TCol | null, dir: SortDir | null) => void;
}

/**
 * Interactive header contents. The enclosing cell owns `aria-sort`
 * (assistive tech tracks the active column there); this button only
 * owns the click + focus ring. The negative left margin lines the
 * chevron up with plain-text column titles in the same row.
 */
export function SortableHeader<TCol extends string>({
  col,
  label,
  sort,
  onSortChange,
}: SortableHeaderProps<TCol>) {
  const active = sort.col === col ? sort.dir : null;
  const [nextCol, nextDir] = nextSort(col, sort);

  return (
    <button
      type="button"
      onClick={() => onSortChange(nextCol, nextDir)}
      className="-ml-1 flex items-center gap-1 rounded-sm px-1 hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      {label}
      <SortIcon active={active} />
    </button>
  );
}
