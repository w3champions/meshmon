import { Link } from "@tanstack/react-router";
import { MapContainer, Marker, Popup, TileLayer } from "react-leaflet";
import type { AgentSummary } from "@/api/hooks/agents";
import type { HealthMatrix } from "@/api/hooks/health-matrix";
import { AgentCard } from "@/components/AgentCard";
import { StatusBadge } from "@/components/StatusBadge";
import type { HealthState } from "@/lib/health";
import { cn } from "@/lib/utils";

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

export function AgentMap({ agents, matrix, className, onMarkerClick }: AgentMapProps) {
  const withCoords = agents.filter(
    (a): a is AgentSummary & { lat: number; lon: number } =>
      typeof a.lat === "number" && typeof a.lon === "number",
  );

  return (
    <div
      className={cn(
        "h-[400px] md:h-[500px] w-full rounded-md border border-border overflow-hidden",
        className,
      )}
      data-testid="agent-map-shell"
    >
      <MapContainer center={[20, 0]} zoom={2} scrollWheelZoom={false} className="h-full w-full">
        <TileLayer
          url="https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png"
          attribution="© OpenStreetMap contributors"
        />
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
                    // biome-ignore lint/suspicious/noExplicitAny: route not yet in Register
                    to={"/agents/$id" as any}
                    // biome-ignore lint/suspicious/noExplicitAny: params follow unregistered route
                    params={{ id: agent.id } as any}
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
