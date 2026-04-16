import { toast as sonnerToast } from "sonner";

type ToastKind = "success" | "error" | "info";

interface ToastInput {
  kind: ToastKind;
  message: string;
  description?: string;
}

/**
 * Adapter over sonner — intentionally NOT a Zustand store.
 *
 * Exposes `useToastStore.getState().pushToast({...})` to mirror the
 * `useAuthStore.getState().clearSession()` shape so the openapi-fetch
 * middleware can call both through the same interface and tests can
 * `vi.mock("@/stores/toast", ...)` the same way they mock the auth store.
 *
 * sonner owns the toast queue; do not try to subscribe via
 * `useToastStore(selector)` — it won't re-render on toast push and will
 * throw at runtime since this is a plain object, not a Zustand hook.
 */
export const useToastStore = {
  getState: () => ({
    pushToast: ({ kind, message, description }: ToastInput) => {
      const method =
        kind === "error"
          ? sonnerToast.error
          : kind === "success"
            ? sonnerToast.success
            : sonnerToast.info;
      method(message, description ? { description } : undefined);
    },
  }),
};
