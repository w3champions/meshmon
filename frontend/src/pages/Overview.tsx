import { useAgents } from "@/api/hooks/agents";
import { useHealthMatrix } from "@/api/hooks/health-matrix";
import { AgentMap } from "@/components/AgentMap";
import { AlertSummaryStrip } from "@/components/AlertSummaryStrip";
import { PathHealthGrid } from "@/components/PathHealthGrid";
import { RecentRoutesTable } from "@/components/RecentRoutesTable";
import { Skeleton } from "@/components/ui/skeleton";

export default function Overview() {
  const { data: agents, isLoading: agentsLoading, isError: agentsError } = useAgents();
  const { data: matrix, isLoading: matrixLoading, isError: matrixError } = useHealthMatrix();

  const loading = agentsLoading || matrixLoading;
  const hasError = agentsError || matrixError;

  return (
    <div className="flex flex-col gap-6 p-6">
      <header>
        <h1 className="text-2xl font-semibold tracking-tight">Overview</h1>
        <p className="text-sm text-muted-foreground">
          Mesh-network health across all registered agents.
        </p>
      </header>
      {hasError && (
        <p role="alert" className="text-sm text-destructive">
          Failed to load one or more data sources.
        </p>
      )}
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
        <RecentRoutesTable />
      </section>
    </div>
  );
}
