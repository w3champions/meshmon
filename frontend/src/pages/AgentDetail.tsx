import { Link } from "@tanstack/react-router";
import { useAgent, useAgents } from "@/api/hooks/agents";
import { useHealthMatrix } from "@/api/hooks/health-matrix";
import { AgentCard } from "@/components/AgentCard";
import { PathHealthGrid } from "@/components/PathHealthGrid";
import { Skeleton } from "@/components/ui/skeleton";
import { agentDetailRoute } from "@/router/index";

export default function AgentDetail() {
  const { id } = agentDetailRoute.useParams();
  const { data: agent, isLoading } = useAgent(id);
  const { data: allAgents = [] } = useAgents();
  const { data: matrix = new Map() } = useHealthMatrix();

  if (isLoading) {
    return <Skeleton className="h-64 w-full" data-testid="agent-detail-skeleton" />;
  }

  if (!agent) {
    return (
      <div className="p-6 flex flex-col gap-3">
        <p className="text-lg font-semibold">Agent not found</p>
        <Link to="/agents" className="text-sm underline underline-offset-2">
          Back to agents
        </Link>
      </div>
    );
  }

  return (
    <div className="p-6 flex flex-col gap-6">
      <AgentCard agent={agent} />
      <section>
        <h2 className="mb-2 text-lg font-semibold">Outgoing paths</h2>
        <div className="overflow-auto">
          <PathHealthGrid agents={allAgents} matrix={matrix} sourceFilter={agent.id} />
        </div>
      </section>
      <section>
        <h2 className="mb-2 text-lg font-semibold">Incoming paths</h2>
        <div className="overflow-auto">
          <PathHealthGrid agents={allAgents} matrix={matrix} targetFilter={agent.id} />
        </div>
      </section>
    </div>
  );
}
