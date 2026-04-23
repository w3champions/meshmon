/**
 * IP→hostname provider seeding: intentionally absent from this hook.
 *
 * Alerts carry `source_hostname` / `target_hostname` as flat sidecars on
 * agent-id labels; the wire does not carry source or target IPs, so the
 * IP→hostname provider cannot be seeded from this hook. Alert rows render
 * the flat hostname fields directly via the documented `AlertRow` convention
 * exception — no `useSeedHostnamesOnResponse` call is needed or appropriate
 * here.
 */
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type AlertSummary = components["schemas"]["AlertSummary"];

export function useAlerts() {
  return useQuery({
    queryKey: ["alerts"],
    queryFn: async () => {
      const { data, error, response } = await api.GET("/api/alerts", {
        params: { query: { active: true } },
      });
      if (response?.status === 503) return [] as AlertSummary[];
      if (error) throw new Error("failed to fetch alerts", { cause: error });
      return (data ?? []) as AlertSummary[];
    },
    refetchInterval: 30_000,
  });
}

export interface AlertSummaryCounts {
  total: number;
  critical: number;
  warning: number;
  info: number;
}

export function useAlertSummary() {
  const q = useAlerts();
  const counts = (q.data ?? []).reduce<AlertSummaryCounts>(
    (acc, a) => {
      acc.total += 1;
      const sev = a.labels?.severity;
      if (sev === "critical") acc.critical += 1;
      else if (sev === "warning") acc.warning += 1;
      else if (sev === "info") acc.info += 1;
      return acc;
    },
    { total: 0, critical: 0, warning: 0, info: 0 },
  );
  return { data: counts, isLoading: q.isLoading, isError: q.isError };
}
