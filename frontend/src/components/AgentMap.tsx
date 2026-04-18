import { Link } from "@tanstack/react-router";
import L from "leaflet";
import iconUrl from "leaflet/dist/images/marker-icon.png";
import iconRetinaUrl from "leaflet/dist/images/marker-icon-2x.png";
import shadowUrl from "leaflet/dist/images/marker-shadow.png";
import { useEffect } from "react";
import { MapContainer, Marker, Popup, TileLayer, useMap } from "react-leaflet";
import type { AgentSummary } from "@/api/hooks/agents";
import type { HealthMatrix } from "@/api/hooks/health-matrix";
import { AgentCard } from "@/components/AgentCard";
import { StatusBadge } from "@/components/StatusBadge";
import type { HealthState } from "@/lib/health";
import { cn } from "@/lib/utils";

// react-leaflet uses Leaflet's Default icon. Vite bundles PNGs as URLs when
// imported; without this override Leaflet tries to load them from the HTML
// document root and 404s on every marker.
L.Icon.Default.mergeOptions({
  iconUrl,
  iconRetinaUrl,
  shadowUrl,
});

interface AgentMapProps {
  agents: AgentSummary[];
  matrix: HealthMatrix;
  className?: string;
  onMarkerClick?: (id: string) => void;
}

const STATE_ORDER: Record<HealthState, number> = {
  unreachable: 3,
  degraded: 2,
  normal: 1,
  stale: 0,
};

function worstOutgoingState(matrix: HealthMatrix, source: string): HealthState {
  let worst: HealthState = "stale";
  for (const entry of matrix.values()) {
    if (entry.source !== source) continue;
    if (STATE_ORDER[entry.state] > STATE_ORDER[worst]) worst = entry.state;
  }
  return worst;
}

function FitToAgents({ points }: { points: Array<[number, number]> }) {
  const map = useMap();
  // Fingerprint the coordinates so the effect only re-fits when the
  // actual set of agent positions changes. Without this the 30 s agents
  // refetch produces a new array reference every poll and the map snaps
  // back, yanking any manual pan/zoom.
  const key = points.map(([la, lo]) => `${la},${lo}`).join("|");
  useEffect(() => {
    if (points.length === 0) return;
    const bounds = L.latLngBounds(points);
    map.fitBounds(bounds, { padding: [40, 40], maxZoom: 5 });
    // biome-ignore lint/correctness/useExhaustiveDependencies: key fingerprints points
  }, [map, key]);
  return null;
}

export function AgentMap({ agents, matrix, className, onMarkerClick }: AgentMapProps) {
  const withCoords = agents.filter(
    (a): a is AgentSummary & { lat: number; lon: number } =>
      typeof a.lat === "number" && typeof a.lon === "number",
  );
  const points: Array<[number, number]> = withCoords.map((a) => [a.lat, a.lon]);

  return (
    <div
      className={cn(
        "h-[400px] md:h-[500px] w-full rounded-md border border-border overflow-hidden",
        className,
      )}
      data-testid="agent-map-shell"
    >
      <MapContainer
        center={[20, 0]}
        zoom={2}
        minZoom={1}
        worldCopyJump
        scrollWheelZoom={false}
        className="h-full w-full"
      >
        <TileLayer
          url="https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png"
          attribution="© OpenStreetMap contributors"
        />
        <FitToAgents points={points} />
        {withCoords.map((agent) => {
          const state = worstOutgoingState(matrix, agent.id);
          return (
            <Marker
              key={agent.id}
              position={[agent.lat, agent.lon]}
              eventHandlers={onMarkerClick ? { click: () => onMarkerClick(agent.id) } : undefined}
            >
              <Popup>
                <div className="flex flex-col gap-2">
                  <StatusBadge state={state === "normal" ? "online" : state} />
                  <AgentCard agent={agent} compact />
                  <Link
                    to={"/agents/$id"}
                    params={{ id: agent.id }}
                    className="text-xs underline underline-offset-2"
                  >
                    View detail
                  </Link>
                </div>
              </Popup>
            </Marker>
          );
        })}
      </MapContainer>
    </div>
  );
}
