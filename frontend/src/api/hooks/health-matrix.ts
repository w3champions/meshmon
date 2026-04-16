import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import { classify, type HealthState } from "@/lib/health";

const PROMQL = `max by (source, target) (max_over_time(meshmon_path_failure_rate[1m]))`;

export interface HealthMatrixEntry {
  source: string;
  target: string;
  failureRate: number;
  state: HealthState;
}

export type HealthMatrix = Map<string, HealthMatrixEntry>;

interface VmInstantResponse {
  status: string;
  data: {
    resultType: string;
    result: Array<{
      metric: Record<string, string>;
      value: [number, string];
    }>;
  };
}

export function useHealthMatrix() {
  return useQuery({
    queryKey: ["health-matrix"],
    queryFn: async (): Promise<HealthMatrix> => {
      const { data, error, response } = await api.GET("/api/metrics/query", {
        params: { query: { query: PROMQL } },
      });
      if (response?.status === 503) return new Map();
      if (error) throw new Error("failed to fetch health matrix", { cause: error });
      const body = data as unknown as VmInstantResponse;
      const out: HealthMatrix = new Map();
      for (const series of body?.data?.result ?? []) {
        const source = series.metric.source;
        const target = series.metric.target;
        if (!source || !target) continue;
        const rate = Number.parseFloat(series.value[1]);
        if (!Number.isFinite(rate)) continue;
        const key = `${source}>${target}`;
        const prev = out.get(key);
        if (prev && prev.failureRate >= rate) continue;
        out.set(key, { source, target, failureRate: rate, state: classify(rate) });
      }
      return out;
    },
    refetchInterval: 30_000,
  });
}
