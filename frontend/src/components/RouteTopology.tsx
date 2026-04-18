import cytoscape, { type LayoutOptions } from "cytoscape";
import dagre from "cytoscape-dagre";
import { useEffect, useRef } from "react";
import type { components } from "@/api/schema.gen";
import type { RouteDiff } from "@/lib/route-diff";
import { cn } from "@/lib/utils";

type HopJson = components["schemas"]["HopJson"];

cytoscape.use(dagre);

// `cytoscape-dagre` extends the base layout options with its own fields
// (`rankDir`, `nodeSep`, `rankSep`, ...). `@types/cytoscape` doesn't know
// about them, so describe the shape locally and widen to `LayoutOptions`
// at the call site.
interface DagreLayoutOptions {
  name: "dagre";
  rankDir?: "LR" | "RL" | "TB" | "BT";
  nodeSep?: number;
  rankSep?: number;
}

export type ColorBy = "latency" | "loss";

interface RouteTopologyProps {
  hops: HopJson[];
  highlightChanges?: RouteDiff["perHop"];
  onNodeClick?: (hop: HopJson) => void;
  colorBy?: ColorBy;
  ariaLabel?: string;
  className?: string;
}

function dominantIp(hop: HopJson): string {
  if (hop.observed_ips.length === 0) return "?";
  let best = hop.observed_ips[0];
  for (const ip of hop.observed_ips) {
    if (ip.freq > best.freq) best = ip;
  }
  return best.ip;
}

function baseClass(hop: HopJson, colorBy: ColorBy): string {
  if (colorBy === "loss") {
    if (hop.loss_pct >= 0.2) return "state-unreachable";
    if (hop.loss_pct >= 0.05) return "state-degraded";
    return "state-normal";
  }
  if (hop.avg_rtt_micros >= 150_000) return "state-unreachable";
  if (hop.avg_rtt_micros >= 50_000) return "state-degraded";
  return "state-normal";
}

function diffClass(kind?: string): string | null {
  switch (kind) {
    case "ip_changed":
    case "latency_changed":
    case "both_changed":
      return "diff-changed";
    case "added":
      return "diff-added";
    case "removed":
      return "diff-removed";
    default:
      return null;
  }
}

export function RouteTopology({
  hops,
  highlightChanges,
  onNodeClick,
  colorBy = "latency",
  ariaLabel,
  className,
}: RouteTopologyProps) {
  const ref = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!ref.current || hops.length === 0) return;
    const elements = [
      ...hops.map((h) => {
        const classes = [baseClass(h, colorBy), diffClass(highlightChanges?.get(h.position)?.kind)]
          .filter((c): c is string => Boolean(c))
          .join(" ");
        return {
          data: { id: String(h.position), label: `#${h.position}\n${dominantIp(h)}` },
          classes,
        };
      }),
      ...hops.slice(1).map((h, i) => ({
        data: {
          id: `e${hops[i].position}-${h.position}`,
          source: String(hops[i].position),
          target: String(h.position),
        },
      })),
    ];
    const cy = cytoscape({
      container: ref.current,
      elements,
      layout: {
        name: "dagre",
        rankDir: "LR",
        nodeSep: 28,
        rankSep: 120,
      } satisfies DagreLayoutOptions as unknown as LayoutOptions,
      style: [
        {
          selector: "node",
          style: {
            shape: "round-rectangle",
            label: "data(label)",
            "text-wrap": "wrap",
            "text-max-width": "120px",
            "text-valign": "center",
            "text-halign": "center",
            width: "label",
            height: "label",
            padding: "8px",
            "font-family": "monospace",
            "font-size": 11,
            color: "#0f172a",
          },
        },
        { selector: "node.state-normal", style: { "background-color": "#22c55e" } },
        { selector: "node.state-degraded", style: { "background-color": "#eab308" } },
        { selector: "node.state-unreachable", style: { "background-color": "#ef4444" } },
        {
          selector: "node.diff-changed",
          style: { "border-width": 3, "border-color": "#eab308" },
        },
        {
          selector: "node.diff-added",
          style: { "border-width": 3, "border-color": "#22c55e" },
        },
        {
          selector: "node.diff-removed",
          style: { "border-width": 3, "border-color": "#ef4444" },
        },
        {
          selector: "edge",
          style: {
            "target-arrow-shape": "triangle",
            "curve-style": "bezier",
            width: 1.5,
            "line-color": "#94a3b8",
            "target-arrow-color": "#94a3b8",
          },
        },
      ],
    });
    if (onNodeClick) {
      cy.on("tap", "node", (evt) => {
        const pos = Number(evt.target.id());
        const hop = hops.find((h) => h.position === pos);
        if (hop) onNodeClick(hop);
      });
    }
    return () => cy.destroy();
  }, [hops, highlightChanges, onNodeClick, colorBy]);

  if (hops.length === 0) {
    return <p className={cn("text-sm text-muted-foreground", className)}>No route data yet.</p>;
  }

  // The graph is rendered into this div by cytoscape as a <canvas>. Expose it
  // as role="img" with an accessible name and a paragraph description linked
  // via aria-describedby — previously we used an sr-only <table> with a
  // <caption>, but Tailwind v4's `.sr-only` clip doesn't cover `<caption>` in
  // all browsers, so the caption leaked as floating text below the topology.
  const descId = `route-topology-desc-${hops[0]?.position ?? "empty"}`;
  const descText = hops
    .map(
      (h) =>
        `Hop ${h.position}: ${dominantIp(h)}, RTT ${(h.avg_rtt_micros / 1000).toFixed(1)} ms, loss ${(h.loss_pct * 100).toFixed(1)}%.`,
    )
    .join(" ");

  return (
    <>
      <div
        ref={ref}
        role="img"
        aria-label={ariaLabel}
        aria-describedby={descId}
        className={cn("h-[360px] md:h-[480px] w-full rounded border", className)}
      />
      <p id={descId} className="sr-only">
        {descText}
      </p>
    </>
  );
}
