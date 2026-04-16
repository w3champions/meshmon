import { Link } from "@tanstack/react-router";
import { useAgent, useAgents } from "@/api/hooks/agents";
import { useHealthMatrix } from "@/api/hooks/health-matrix";
import { AgentCard } from "@/components/AgentCard";
import { PathHealthGrid } from "@/components/PathHealthGrid";
import { Skeleton } from "@/components/ui/skeleton";
import { agentDetailRoute } from "@/router/index";

export default function AgentDetail() {
  const { id } = agentDetailRoute.useParams();
  const { data: agent, isLoading, isError: agentError } = useAgent(id);
  const { data: allAgents = [], isLoading: agentsLoading, isError: agentsError } = useAgents();
  const {
    data: matrix = new Map(),
    isLoading: matrixLoading,
    isError: matrixError,
  } = useHealthMatrix();

  if (isLoading || agentsLoading || matrixLoading) {
    return <Skeleton className="h-64 w-full" data-testid="agent-detail-skeleton" />;
  }

  const hasError = agentError || agentsError || matrixError;

  if (agentError && !agent) {
    return (
      <div className="p-6 flex flex-col gap-3">
        <p role="alert" className="text-sm text-destructive">
          Failed to load agent
        </p>
        <Link to="/agents" className="text-sm underline underline-offset-2">
          Back to agents
        </Link>
      </div>
    );
  }

  if (!isLoading && !agentError && !agent) {
    return (
      <div className="p-6 flex flex-col gap-3">
        <h2 className="text-lg font-semibold">Agent not found</h2>
        <Link to="/agents" className="text-sm underline underline-offset-2">
          Back to agents
        </Link>
      </div>
    );
  }

  // At this point agent must be defined: both error+null and !error+null branches returned early.
  // biome-ignore lint/style/noNonNullAssertion: narrowed by early returns above
  const resolvedAgent = agent!;

  return (
    <div className="p-6 flex flex-col gap-6">
      {hasError && (
        <p role="alert" className="text-sm text-destructive">
          Failed to load one or more data sources.
        </p>
      )}
      <AgentCard agent={resolvedAgent} />
      <section>
        <h2 className="mb-2 text-lg font-semibold">Outgoing paths</h2>
        <div className="overflow-auto">
          <PathHealthGrid agents={allAgents} matrix={matrix} sourceFilter={resolvedAgent.id} />
        </div>
      </section>
      <section>
        <h2 className="mb-2 text-lg font-semibold">Incoming paths</h2>
        <div className="overflow-auto">
          <PathHealthGrid agents={allAgents} matrix={matrix} targetFilter={resolvedAgent.id} />
        </div>
      </section>
    </div>
  );
}
