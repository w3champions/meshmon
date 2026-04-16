import { useAgents } from "@/api/hooks/agents";
import { AgentsTable } from "@/components/AgentsTable";
import { Skeleton } from "@/components/ui/skeleton";

export default function AgentsList() {
  const { data: agents, isLoading, isError } = useAgents();

  return (
    <div className="flex flex-col gap-4 p-6">
      <header>
        <h1 className="text-2xl font-semibold tracking-tight">Agents</h1>
        <p className="text-sm text-muted-foreground">Every agent registered with this mesh.</p>
      </header>

      {isLoading && <Skeleton className="h-64 w-full" data-testid="agents-skeleton" />}
      {isError && (
        <p className="text-sm text-destructive" role="alert">
          Failed to load agents
        </p>
      )}
      {agents !== undefined && agents.length === 0 && (
        <p className="text-sm text-muted-foreground">No agents registered yet</p>
      )}
      {agents !== undefined && agents.length > 0 && <AgentsTable agents={agents} />}
    </div>
  );
}
