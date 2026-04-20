import { memo, useEffect, useRef } from "react";
import { buildGrafanaSoloUrl } from "@/lib/grafana-panels";
import { cn } from "@/lib/utils";
import { useUiStore } from "@/stores/ui";

interface GrafanaPanelProps {
  /** Dashboard UID — matches the Grafana dashboard's `uid` (e.g., `"meshmon-path"`).
   *  Grafana proxies are mounted at `/grafana`; the panel builds `/grafana/d-solo/<uid>?…`. */
  dashboard: string;
  panelId: number;
  vars: Record<string, string>;
  from: string;
  to: string;
  title: string;
  className?: string;
  theme?: "light" | "dark";
}

/**
 * Shallow-compare two `vars` records. Two objects are equal when they have
 * the same keys and each key holds the same primitive string value. This is
 * the only meaningful equality for a `Record<string, string>` and avoids
 * treating every parent re-render (which allocates a fresh object literal)
 * as a prop change.
 */
function shallowEqVars(a: Record<string, string>, b: Record<string, string>): boolean {
  if (a === b) return true;
  const aKeys = Object.keys(a);
  const bKeys = Object.keys(b);
  if (aKeys.length !== bKeys.length) return false;
  for (const k of aKeys) {
    if (a[k] !== b[k]) return false;
  }
  return true;
}

function GrafanaPanelImpl({
  dashboard,
  panelId,
  vars,
  from,
  to,
  title,
  className,
  theme: themeProp,
}: GrafanaPanelProps) {
  // Fall back to the app theme when the caller doesn't pin one (e.g. the
  // interactive path view). Print surfaces pass `theme="light"` explicitly
  // so they stay readable on paper regardless of the app theme.
  const appTheme = useUiStore((s) => s.theme);
  const resolvedTheme = themeProp ?? appTheme;
  const src = buildGrafanaSoloUrl({
    uid: dashboard,
    panelId,
    vars,
    from,
    to,
    theme: resolvedTheme,
  });
  const ref = useRef<HTMLIFrameElement | null>(null);

  // Update the iframe's `src` imperatively instead of letting React replace
  // the `<iframe>` DOM node. Changing the `src` attribute on a rendered
  // iframe swaps its document in place (preserving scroll / history), while
  // remounting the element triggers a fresh network round-trip + visible
  // flicker — exactly what users perceived as a "full page refresh" on
  // protocol/range toggles.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    // Compare against the resolved absolute URL the browser exposes on
    // `HTMLIFrameElement.src`; the input `src` is typically relative.
    const resolved = new URL(src, window.location.href).href;
    if (el.src !== resolved) {
      el.src = src;
    }
  }, [src]);

  return (
    <iframe
      ref={ref}
      title={title}
      // Initial mount only; subsequent `src` changes flow through the effect
      // above so the DOM node itself is preserved across prop updates.
      src={src}
      className={cn("h-56 w-full rounded border", className)}
      loading="lazy"
      // Don't leak the full path + query (which includes agent IDs) to the
      // embedded Grafana when the panel first loads. Same-origin now, but
      // the policy still blocks leaking agent IDs into a Grafana access log.
      referrerPolicy="no-referrer"
      // Grafana's solo panel needs same-origin access (for its own cookies /
      // API calls) and script execution, but nothing else. Keep the sandbox
      // tight so the embedded page can't navigate the top frame or spawn
      // popups.
      sandbox="allow-same-origin allow-scripts"
    />
  );
}

export const GrafanaPanel = memo(GrafanaPanelImpl, (prev, next) => {
  // Shallow-compare each prop so the panel only re-renders when something
  // that actually affects the rendered iframe changes. `vars` is a nested
  // `Record<string, string>` — compare its entries, not its identity, so a
  // fresh object literal from the parent doesn't force an unnecessary
  // render. The `theme` prop compare still works when the caller omits it
  // (both sides `undefined`); app-theme changes reach the component via
  // the Zustand subscription inside `GrafanaPanelImpl`, which bypasses
  // memo — so callers that don't pin `theme` still re-render when the
  // store flips.
  return (
    prev.dashboard === next.dashboard &&
    prev.panelId === next.panelId &&
    prev.from === next.from &&
    prev.to === next.to &&
    prev.theme === next.theme &&
    prev.title === next.title &&
    prev.className === next.className &&
    shallowEqVars(prev.vars, next.vars)
  );
});
