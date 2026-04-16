import { formatDistanceToNowStrict } from "date-fns";
import type { AgentSummary } from "@/api/hooks/agents";
import { StatusBadge } from "@/components/StatusBadge";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { isStale } from "@/lib/health";

interface AgentCardProps {
  agent: AgentSummary;
  compact?: boolean;
}

export function AgentCard({ agent, compact = false }: AgentCardProps) {
  const stale = isStale(agent.last_seen_at);

  return (
    <Card>
      <CardHeader>
        <div className="flex items-start justify-between gap-2">
          <div>
            <CardTitle>{agent.display_name}</CardTitle>
            <CardDescription>{agent.id}</CardDescription>
          </div>
          <StatusBadge state={stale ? "stale" : "online"} />
        </div>
      </CardHeader>

      <CardContent>
        <div className="space-y-1 text-sm">
          <div>
            <span className="text-muted-foreground">IP: </span>
            <span>{agent.ip}</span>
          </div>
          {agent.location != null && (
            <div>
              <span className="text-muted-foreground">Location: </span>
              <span>{agent.location}</span>
            </div>
          )}
          {agent.lat != null && agent.lon != null && (
            <div>
              <span className="text-muted-foreground">Coordinates: </span>
              <span>
                {agent.lat}, {agent.lon}
              </span>
            </div>
          )}
        </div>
      </CardContent>

      {!compact && (
        <CardFooter className="text-xs text-muted-foreground">
          <span>
            Last seen{" "}
            {formatDistanceToNowStrict(new Date(agent.last_seen_at), {
              addSuffix: true,
            })}
          </span>
          {agent.agent_version != null && <span>&nbsp;&middot;&nbsp;v{agent.agent_version}</span>}
        </CardFooter>
      )}
    </Card>
  );
}
