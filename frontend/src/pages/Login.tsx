import { zodResolver } from "@hookform/resolvers/zod";
import { useMutation } from "@tanstack/react-query";
import { useForm } from "react-hook-form";
import { z } from "zod";
import { api } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useAuthStore } from "@/stores/auth";

const schema = z.object({
  username: z.string().min(1, "Username is required"),
  password: z.string().min(1, "Password is required"),
});

type FormValues = z.infer<typeof schema>;

export default function Login() {
  const {
    register,
    handleSubmit,
    formState: { errors },
    setError,
  } = useForm<FormValues>({ resolver: zodResolver(schema) });

  const mutation = useMutation({
    mutationFn: async (values: FormValues) => {
      const { data, error, response } = await api.POST("/api/auth/login", {
        body: values,
      });
      if (response.status === 429) {
        const retryAfter = response.headers.get("Retry-After");
        const seconds = retryAfter ? Number.parseInt(retryAfter, 10) : 60;
        throw new Error(
          `Too many attempts. Try again in ${Number.isFinite(seconds) ? seconds : 60}s`,
        );
      }
      if (response.status === 401) {
        throw new Error("Invalid credentials");
      }
      if (error) {
        throw new Error("Something went wrong. Please try again later.");
      }
      if (!data) {
        throw new Error("unexpected empty response");
      }
      return data;
    },
    onSuccess: ({ username }) => {
      useAuthStore.getState().setSession({ username });
      // Open-redirect guard: only allow same-origin relative paths. Reject
      // protocol-relative ("//evil.com"), absolute ("https://evil.com"), and
      // URI-scheme ("javascript:…") values an attacker might smuggle via
      // `?returnTo=`. Don't "simplify" this away.
      const raw = new URLSearchParams(window.location.search).get("returnTo") ?? "/";
      const returnTo = raw.startsWith("/") && !raw.startsWith("//") ? raw : "/";
      window.location.assign(returnTo);
    },
    onError: (err: Error) => {
      setError("root", { message: err.message });
    },
  });

  const onSubmit = handleSubmit((values) => mutation.mutate(values));

  return (
    <div className="min-h-screen flex items-center justify-center p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle>Sign in</CardTitle>
        </CardHeader>
        <CardContent>
          <form onSubmit={onSubmit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="username">Username</Label>
              <Input id="username" autoComplete="username" {...register("username")} />
              {errors.username && (
                <p className="text-sm text-destructive">{errors.username.message}</p>
              )}
            </div>
            <div className="space-y-2">
              <Label htmlFor="password">Password</Label>
              <Input
                id="password"
                type="password"
                autoComplete="current-password"
                {...register("password")}
              />
              {errors.password && (
                <p className="text-sm text-destructive">{errors.password.message}</p>
              )}
            </div>
            {errors.root && (
              <p className="text-sm text-destructive" role="alert">
                {errors.root.message}
              </p>
            )}
            <Button type="submit" className="w-full" disabled={mutation.isPending}>
              {mutation.isPending ? "Signing in…" : "Sign in"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
