import { useAgents } from "@/api/hooks/agents";
import { useHealthMatrix } from "@/api/hooks/health-matrix";
import { AgentMap } from "@/components/AgentMap";
import { AlertSummaryStrip } from "@/components/AlertSummaryStrip";
import { PathHealthGrid } from "@/components/PathHealthGrid";
import { RecentRoutesTable } from "@/components/RecentRoutesTable";
import { Skeleton } from "@/components/ui/skeleton";

export default function Overview() {
  const { data: agents, isLoading: agentsLoading } = useAgents();
  const { data: matrix, isLoading: matrixLoading } = useHealthMatrix();

  const loading = agentsLoading || matrixLoading;
  const empty = (agents ?? []).length === 0;

  return (
    <div className="flex flex-col gap-6 p-6">
      <header>
        <h1 className="text-2xl font-semibold tracking-tight">Overview</h1>
        <p className="text-sm text-muted-foreground">
          Mesh-network health across all registered agents.
        </p>
      </header>
      <AlertSummaryStrip />
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        {loading ? (
          <Skeleton data-testid="map-skeleton" className="h-[400px] md:h-[500px] w-full" />
        ) : (
          <AgentMap agents={agents ?? []} matrix={matrix ?? new Map()} />
        )}
        {loading ? (
          <Skeleton data-testid="grid-skeleton" className="h-[400px] w-full" />
        ) : (
          <div className="overflow-auto">
            <PathHealthGrid agents={agents ?? []} matrix={matrix ?? new Map()} />
          </div>
        )}
      </div>
      <section className="flex flex-col gap-2">
        <h2 className="text-lg font-semibold">Recent route changes</h2>
        {empty && !loading ? (
          <p className="text-sm text-muted-foreground">No recent route changes</p>
        ) : (
          <RecentRoutesTable />
        )}
      </section>
    </div>
  );
}
