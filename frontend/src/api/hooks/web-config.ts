import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";

export function useWebConfig() {
  return useQuery({
    queryKey: ["web-config"],
    queryFn: async () => {
      const { data, error } = await api.GET("/api/web-config");
      if (error) {
        throw new Error("failed to fetch web config");
      }
      if (!data) {
        throw new Error("empty response");
      }
      return data;
    },
    staleTime: Number.POSITIVE_INFINITY,
    retry: false,
  });
}
