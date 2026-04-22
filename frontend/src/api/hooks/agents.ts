import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import type { components } from "@/api/schema.gen";
import { useSeedHostnamesOnResponse } from "@/components/ip-hostname";

export type AgentSummary = components["schemas"]["AgentSummary"];

export function useAgents() {
  const query = useQuery({
    queryKey: ["agents"],
    queryFn: async () => {
      const { data, error } = await api.GET("/api/agents");
      if (error) throw new Error("failed to fetch agents", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
    refetchInterval: 30_000,
  });
  // Seed the shared hostname map from every response. The agents endpoint
  // returns an array of `AgentSummary` carrying `{ ip, hostname? }`; the
  // provider deduplicates + ignores `undefined`, so repeated polls are safe.
  useSeedHostnamesOnResponse(query.data, (agents) => agents);
  return query;
}

export function useAgent(id: string) {
  const query = useQuery({
    queryKey: ["agent", id],
    queryFn: async () => {
      const { data, error, response } = await api.GET("/api/agents/{id}", {
        params: { path: { id } },
      });
      // Check 404 before error — openapi-fetch fills error for non-2xx.
      if (response?.status === 404) return null;
      if (error) throw new Error("failed to fetch agent", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
    refetchInterval: 30_000,
  });
  // Single-agent response still seeds through the shared provider so the
  // drilldown's IP renders the hostname on first paint.
  useSeedHostnamesOnResponse(query.data, (agent) => (agent ? [agent] : []));
  return query;
}
