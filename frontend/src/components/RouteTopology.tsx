import cytoscape, { type LayoutOptions } from "cytoscape";
import dagre from "cytoscape-dagre";
import { useEffect, useId, useMemo, useRef } from "react";
import type { components } from "@/api/schema.gen";
import { useIpHostnames } from "@/components/ip-hostname";
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

/**
 * Middle-truncate a hostname for Cytoscape canvas nodes (which have limited
 * horizontal space). Full value remains available in the hop-detail card panel.
 *
 * - ≤ 24 code points → returned as-is.
 * - > 24 code points → first 10 + `…` + last 10 code points.
 *
 * Iterates by Unicode code point (not UTF-16 code units) so a non-BMP
 * character (emoji, technical symbol) straddling the cut boundary is
 * never split across its surrogate pair — a lone surrogate would render
 * as U+FFFD and can break downstream consumers such as
 * `encodeURIComponent` / `JSON.stringify`. PTR records are ASCII-
 * compatible today, but hostnames arrive via IDN clients and operator
 * annotations, so robustness matters.
 */
export function truncateHostname(name: string): string {
  const codePoints = [...name];
  if (codePoints.length <= 24) return name;
  return `${codePoints.slice(0, 10).join("")}…${codePoints.slice(-10).join("")}`;
}

function baseClass(hop: HopJson, colorBy: ColorBy): string {
  if (colorBy === "loss") {
    if (hop.loss_ratio >= 0.2) return "state-unreachable";
    if (hop.loss_ratio >= 0.05) return "state-degraded";
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

const dagreOptions: DagreLayoutOptions = {
  name: "dagre",
  rankDir: "LR",
  nodeSep: 28,
  rankSep: 120,
};

export function RouteTopology({
  hops,
  highlightChanges,
  onNodeClick,
  colorBy = "latency",
  ariaLabel,
  className,
}: RouteTopologyProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const cyRef = useRef<cytoscape.Core | null>(null);

  const hopIps = useMemo(() => hops.map(dominantIp).filter((ip) => ip !== "?"), [hops]);
  const hostnames = useIpHostnames(hopIps);
  const hostnamesRef = useRef(hostnames);
  hostnamesRef.current = hostnames;

  // Label builder reads the latest hostnames via ref so the build
  // effect's dep list does NOT include hostnames. Rationale (keep
  // this comment in the source code): adding `hostnames` to the build
  // effect's deps would rebuild Cytoscape on every hostname arrival
  // and violate the T38-class in-place-update contract. The ref
  // always points at the latest map; the initial build uses whatever
  // is already seeded; late arrivals ride the second effect below.
  const labelFor = (h: HopJson): string => {
    const ip = dominantIp(h);
    const hn = hostnamesRef.current.get(ip);
    return typeof hn === "string" && hn.length > 0
      ? `#${h.position}\n${ip}\n${truncateHostname(hn)}`
      : `#${h.position}\n${ip}`;
  };

  // Build / rebuild on structural changes only.
  // biome-ignore lint/correctness/useExhaustiveDependencies: `labelFor` reads `hostnamesRef.current` (always latest) intentionally — adding `hostnames` here would rebuild Cy on every hostname arrival, violating the T38-class in-place-update contract.
  useEffect(() => {
    if (!containerRef.current || hops.length === 0) return;
    const elements = [
      ...hops.map((h) => {
        const classes = [baseClass(h, colorBy), diffClass(highlightChanges?.get(h.position)?.kind)]
          .filter((c): c is string => Boolean(c))
          .join(" ");
        return {
          data: { id: String(h.position), label: labelFor(h) },
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
      container: containerRef.current,
      elements,
      layout: dagreOptions as unknown as LayoutOptions,
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
    cyRef.current = cy;
    return () => {
      cy.destroy();
      cyRef.current = null;
    };
  }, [hops, highlightChanges, onNodeClick, colorBy]);

  // In-place label update on hostname events — no rebuild, no layout.
  // Guard on `!cy` handles the impossible mount-race (build effect
  // runs on the same render tick that sets cyRef.current, so this
  // effect only needs to handle subsequent updates; the initial
  // labels already reflect the latest hostnames from the build
  // effect's labelFor call).
  //
  // `labelFor` reads `hostnamesRef.current` (the ref pattern) and is
  // intentionally excluded from the dep list. `hostnames` identity change
  // is the signal; `labelFor` is a stable renderer that reads the ref.
  // biome-ignore lint/correctness/useExhaustiveDependencies: `labelFor` uses `hostnamesRef` ref-pattern; `hostnames` identity change is the correct trigger, not `labelFor` reference equality.
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    for (const h of hops) {
      cy.getElementById(String(h.position)).data("label", labelFor(h));
    }
  }, [hops, hostnames]);

  const reactId = useId();

  if (hops.length === 0) {
    return <p className={cn("text-sm text-muted-foreground", className)}>No route data yet.</p>;
  }

  // The graph is rendered into this div by cytoscape as a <canvas>. Expose it
  // as role="img" with an accessible name and a paragraph description linked
  // via aria-describedby — previously we used an sr-only <table> with a
  // <caption>, but Tailwind v4's `.sr-only` clip doesn't cover `<caption>` in
  // all browsers, so the caption leaked as floating text below the topology.
  const descId = `route-topology-desc-${reactId}`;
  const descText = hops
    .map(
      (h) =>
        `Hop ${h.position}: ${dominantIp(h)}, RTT ${(h.avg_rtt_micros / 1000).toFixed(1)} ms, loss ${(h.loss_ratio * 100).toFixed(1)}%.`,
    )
    .join(" ");

  return (
    <>
      <div
        ref={containerRef}
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
