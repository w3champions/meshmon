import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { api } from "@/api/client";
import type { CatalogueEntry } from "@/api/hooks/catalogue";
import { EntryDrawer } from "./EntryDrawer";

// ---------------------------------------------------------------------------
// Context shape
// ---------------------------------------------------------------------------

interface CatalogueDrawerContextValue {
  /** Open the EntryDrawer for the given IP address. No-op if IP not in catalogue. */
  open: (ip: string) => void;
  /** Programmatically close the drawer. */
  close: () => void;
}

const CatalogueDrawerContext = createContext<CatalogueDrawerContextValue | null>(null);

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

interface CatalogueDrawerOverlayProps {
  children: ReactNode;
}

/**
 * Provides a page-level EntryDrawer that any descendant can open by IP via
 * `useCatalogueDrawer()`. Mount once per page (or at the campaigns route
 * root); the drawer renders at z-index above the per-B drawers.
 */
export function CatalogueDrawerOverlay({ children }: CatalogueDrawerOverlayProps) {
  const [entry, setEntry] = useState<CatalogueEntry | undefined>(undefined);
  const fetchingRef = useRef<string | null>(null);

  const open = useCallback(async (ip: string) => {
    if (fetchingRef.current === ip) return;
    fetchingRef.current = ip;
    try {
      // Look up the catalogue entry by IP using the ip_prefix filter.
      // A bare host IP matches its own /32 or /128 entry via the
      // `ip <<= $prefix` operator on the server.
      const { data, error } = await api.GET("/api/catalogue", {
        params: {
          query: {
            ip_prefix: ip,
            limit: 1,
          },
        },
      });
      if (error || !data) {
        fetchingRef.current = null;
        return;
      }
      const match = data.entries.find((e) => e.ip === ip) ?? data.entries[0];
      if (match) setEntry(match);
    } catch {
      // Best-effort; don't crash the caller on network failure.
    } finally {
      fetchingRef.current = null;
    }
  }, []);

  const close = useCallback(() => setEntry(undefined), []);

  return (
    <CatalogueDrawerContext.Provider value={{ open, close }}>
      {children}
      {/* z-[60] positions this above per-B drawers (z-50) */}
      <div className="z-[60]">
        <EntryDrawer entry={entry} onClose={close} />
      </div>
    </CatalogueDrawerContext.Provider>
  );
}

// ---------------------------------------------------------------------------
// Consumer hook
// ---------------------------------------------------------------------------

/**
 * Returns `{ open(ip), close() }` to interact with the nearest
 * `<CatalogueDrawerOverlay>`. Throws if mounted outside the provider tree.
 */
export function useCatalogueDrawer(): CatalogueDrawerContextValue {
  const ctx = useContext(CatalogueDrawerContext);
  if (!ctx) {
    throw new Error("useCatalogueDrawer must be used inside <CatalogueDrawerOverlay>");
  }
  return ctx;
}
