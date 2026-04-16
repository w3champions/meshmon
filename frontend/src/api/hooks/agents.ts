import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type AgentSummary = components["schemas"]["AgentSummary"];

export function useAgents() {
  return useQuery({
    queryKey: ["agents"],
    queryFn: async () => {
      const { data, error } = await api.GET("/api/agents");
      if (error) throw new Error("failed to fetch agents", { cause: error });
      if (!data) throw new Error("empty response");
      return data;
    },
    refetchInterval: 30_000,
  });
}

export function useAgent(id: string) {
  return useQuery({
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
}
