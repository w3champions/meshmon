/**
 * Stacked bar visualising the route-mix of an edge-candidate evaluation:
 * direct (A→B), one-hop (A→X→B), and two-hop (A→X→Y→B) shares.
 *
 * Uses the Okabe-Ito CVD-safe palette via the global CSS variables defined in
 * `globals.css` (`--route-direct`, `--route-1hop`, `--route-2hop`).
 */

type Props = {
  /** Fraction of routes that are direct (0..1). */
  direct: number;
  /** Fraction of routes that are one-hop (0..1). */
  oneHop: number;
  /** Fraction of routes that are two-hop (0..1). */
  twoHop: number;
};

export function RouteMixBar({ direct, oneHop, twoHop }: Props) {
  const total = direct + oneHop + twoHop;
  if (total === 0) {
    return (
      <div
        role="img"
        aria-label="no reachable destinations"
        className="route-mix-bar route-mix-bar-empty"
      />
    );
  }
  const pct = (v: number) => `${Math.round(v * 100)}%`;
  const ariaLabel = `${pct(direct)} direct, ${pct(oneHop)} one-hop, ${pct(twoHop)} two-hop`;
  return (
    <div role="img" aria-label={ariaLabel} className="route-mix-bar">
      <div
        data-segment="direct"
        style={{ width: pct(direct), background: "var(--route-direct)" }}
      />
      <div
        data-segment="onehop"
        style={{ width: pct(oneHop), background: "var(--route-1hop)" }}
      />
      <div
        data-segment="twohop"
        style={{ width: pct(twoHop), background: "var(--route-2hop)" }}
      />
    </div>
  );
}
