import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";

export function useSession() {
  return useQuery({
    queryKey: ["session"],
    queryFn: async () => {
      const { data, error } = await api.GET("/api/session");
      if (error) {
        throw new Error("failed to fetch session");
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
