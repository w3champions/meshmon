import { useMemo, useState } from "react";
import { type AlertSummary, useAlerts } from "@/api/hooks/alerts";
import { AlertRow } from "@/components/AlertRow";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import {
  type AlertFilter,
  defaultAlertFilter,
  filterAlerts,
  type ProtocolFilter,
  type Severity,
  uniqueCategories,
} from "@/lib/alerts-filter";

export default function Alerts() {
  const alertsQ = useAlerts();
  const [filter, setFilter] = useState<AlertFilter>(defaultAlertFilter());

  const alerts = alertsQ.data ?? [];
  const categories = useMemo(() => uniqueCategories(alerts), [alerts]);
  const visible = useMemo(() => filterAlerts(alerts, filter), [alerts, filter]);

  if (alertsQ.isLoading) {
    return (
      <div className="p-6">
        <Skeleton className="h-64 w-full" data-testid="alerts-skeleton" />
      </div>
    );
  }
  if (alertsQ.isError) {
    return (
      <p role="alert" className="p-6 text-sm text-destructive">
        Failed to load alerts.
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-6 p-6">
      <header className="flex flex-wrap items-end gap-4">
        <div className="flex flex-col gap-1">
          <Label htmlFor="filter-severity">Severity</Label>
          <Select
            value={filter.severity}
            onValueChange={(v) => setFilter({ ...filter, severity: v as Severity })}
          >
            <SelectTrigger id="filter-severity" aria-label="Severity" className="w-36">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All</SelectItem>
              <SelectItem value="critical">Critical</SelectItem>
              <SelectItem value="warning">Warning</SelectItem>
              <SelectItem value="info">Info</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="filter-category">Category</Label>
          <Select
            value={filter.category}
            onValueChange={(v) => setFilter({ ...filter, category: v })}
          >
            <SelectTrigger id="filter-category" aria-label="Category" className="w-36">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All</SelectItem>
              {categories.map((c) => (
                <SelectItem key={c} value={c}>
                  {c}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="filter-protocol">Protocol</Label>
          <Select
            value={filter.protocol}
            onValueChange={(v) => setFilter({ ...filter, protocol: v as ProtocolFilter })}
          >
            <SelectTrigger id="filter-protocol" aria-label="Protocol" className="w-36">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All</SelectItem>
              <SelectItem value="icmp">ICMP</SelectItem>
              <SelectItem value="udp">UDP</SelectItem>
              <SelectItem value="tcp">TCP</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="filter-source">Source</Label>
          <Input
            id="filter-source"
            value={filter.source}
            onChange={(e) => setFilter({ ...filter, source: e.target.value })}
            placeholder="substring…"
            className="w-40"
          />
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="filter-target">Target</Label>
          <Input
            id="filter-target"
            value={filter.target}
            onChange={(e) => setFilter({ ...filter, target: e.target.value })}
            placeholder="substring…"
            className="w-40"
          />
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="filter-text">Search</Label>
          <Input
            id="filter-text"
            value={filter.text}
            onChange={(e) => setFilter({ ...filter, text: e.target.value })}
            placeholder="alertname / summary / description…"
            className="w-64"
          />
        </div>
      </header>

      {alerts.length === 0 ? (
        <p className="text-sm text-muted-foreground">No active alerts.</p>
      ) : visible.length === 0 ? (
        <p className="text-sm text-muted-foreground">No alerts match the current filters.</p>
      ) : (
        <div className="grid gap-3">
          {visible.map((a: AlertSummary) => (
            <AlertRow key={a.fingerprint} alert={a} />
          ))}
        </div>
      )}
    </div>
  );
}
