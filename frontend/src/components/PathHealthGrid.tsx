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
      {/* biome-ignore lint/a11y/useSemanticElements: role="grid" is intentional on a CSS grid; <table> would break the layout */}
      <div
        role="grid"
        aria-label="Path health matrix"
        className={cn("inline-grid gap-1", className)}
        style={{
          gridTemplateColumns: `minmax(4rem, auto) repeat(${cols.length}, 1.5rem)`,
        }}
      >
        {/* Header row: top-left corner + column headers */}
        {/* biome-ignore lint/a11y/useFocusableInteractive: role="row" is a structural ARIA grouping inside a CSS grid; focusability is on the cell children */}
        {/* biome-ignore lint/a11y/useSemanticElements: role="row" on div is intentional — <tr> cannot be used in a CSS grid layout */}
        <div role="row" style={{ display: "contents" }}>
          {/* Top-left corner */}
          <div />
          {/* Column headers */}
          {cols.map((col) => (
            /* biome-ignore lint/a11y/useSemanticElements: role="columnheader" on div is intentional for ARIA grid; <th> cannot be used in CSS grid layout */
            <div
              key={`h-${col}`}
              role="columnheader"
              tabIndex={-1}
              className="text-xs font-mono rotate-[-60deg] origin-bottom-left whitespace-nowrap text-muted-foreground"
              data-testid="col-header"
            >
              {col}
            </div>
          ))}
        </div>
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
    /* biome-ignore lint/a11y/useFocusableInteractive: role="row" is a structural ARIA grouping inside a CSS grid; focusability is on the cell children */
    /* biome-ignore lint/a11y/useSemanticElements: role="row" on div is intentional — <tr> cannot be used in a CSS grid layout */
    <div role="row" style={{ display: "contents" }}>
      {/* biome-ignore lint/a11y/useSemanticElements: role="rowheader" on div is intentional for ARIA grid; <th> cannot be used in CSS grid layout */}
      <div
        role="rowheader"
        tabIndex={-1}
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
    </div>
  );
}
