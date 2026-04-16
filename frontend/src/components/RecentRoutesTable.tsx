import { Link } from "@tanstack/react-router";
import { formatDistanceToNowStrict } from "date-fns";
import { useRecentRouteChanges } from "@/api/hooks/recent-routes";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { cn } from "@/lib/utils";

interface RecentRoutesTableProps {
  className?: string;
  limit?: number;
}

export function RecentRoutesTable({ className, limit = 10 }: RecentRoutesTableProps) {
  const { data, isLoading } = useRecentRouteChanges(limit);

  if (isLoading) {
    return (
      <Skeleton className={cn("h-40 w-full", className)} data-testid="recent-routes-skeleton" />
    );
  }

  const rows = data ?? [];
  if (rows.length === 0) {
    return (
      <p className={cn("text-sm text-muted-foreground", className)}>No recent route changes</p>
    );
  }

  return (
    <Table className={className}>
      <TableHeader>
        <TableRow>
          <TableHead>Pair</TableHead>
          <TableHead>Protocol</TableHead>
          <TableHead>Observed</TableHead>
          <TableHead className="text-right">Action</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((row) => (
          <TableRow key={row.id}>
            <TableCell className="font-mono text-xs">
              {row.source_id} → {row.target_id}
            </TableCell>
            <TableCell className="uppercase text-xs">{row.protocol}</TableCell>
            <TableCell className="text-xs text-muted-foreground">
              {formatDistanceToNowStrict(new Date(row.observed_at), {
                addSuffix: true,
              })}
            </TableCell>
            <TableCell className="text-right">
              {/* Route /paths/$source/$target is not yet registered — cast until the paths page is added */}
              <Link
                // biome-ignore lint/suspicious/noExplicitAny: route not yet in Register
                to={"/paths/$source/$target" as any}
                // biome-ignore lint/suspicious/noExplicitAny: params follow unregistered route
                params={{ source: row.source_id, target: row.target_id } as any}
                className="text-sm underline underline-offset-2"
              >
                view
              </Link>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}
