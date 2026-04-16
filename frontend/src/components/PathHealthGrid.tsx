import type { AgentSummary } from "@/api/hooks/agents";
import type { HealthMatrix } from "@/api/hooks/health-matrix";
import { PathHealthCell } from "@/components/PathHealthCell";
import { TooltipProvider } from "@/components/ui/tooltip";
import { classify } from "@/lib/health";
import { cn } from "@/lib/utils";

interface PathHealthGridProps {
  agents: AgentSummary[];
  matrix: HealthMatrix;
  sourceFilter?: string;
  targetFilter?: string;
  className?: string;
}

export function PathHealthGrid({
  agents,
  matrix,
  sourceFilter,
  targetFilter,
  className,
}: PathHealthGridProps) {
  if (agents.length === 0) {
    return (
      <p className={cn("text-sm text-muted-foreground", className)}>No agents registered yet.</p>
    );
  }

  const sortedIds = [...agents].map((a) => a.id).sort();
  const rows = sourceFilter ? sortedIds.filter((id) => id === sourceFilter) : sortedIds;
  const cols = targetFilter ? sortedIds.filter((id) => id === targetFilter) : sortedIds;

  return (
    <TooltipProvider delayDuration={150}>
      <div
        className={cn("inline-grid gap-1", className)}
        style={{
          gridTemplateColumns: `minmax(4rem, auto) repeat(${cols.length}, 1.5rem)`,
        }}
      >
        {/* Top-left corner */}
        <div />
        {/* Column headers */}
        {cols.map((col) => (
          <div
            key={`h-${col}`}
            className="text-xs font-mono rotate-[-60deg] origin-bottom-left whitespace-nowrap text-muted-foreground"
            data-testid="col-header"
          >
            {col}
          </div>
        ))}
        {rows.map((source) => (
          <Row key={source} source={source} cols={cols} matrix={matrix} />
        ))}
      </div>
    </TooltipProvider>
  );
}

interface RowProps {
  source: string;
  cols: string[];
  matrix: HealthMatrix;
}

function Row({ source, cols, matrix }: RowProps) {
  return (
    <>
      <div
        className="text-xs font-mono text-right pr-2 text-muted-foreground truncate"
        data-testid="row-header"
      >
        {source}
      </div>
      {cols.map((target) => {
        const entry = matrix.get(`${source}>${target}`);
        const state = entry?.state ?? classify(undefined);
        return (
          <PathHealthCell
            key={`${source}>${target}`}
            source={source}
            target={target}
            state={state}
            failureRate={entry?.failureRate}
          />
        );
      })}
    </>
  );
}
