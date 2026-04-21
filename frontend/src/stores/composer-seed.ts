import { create } from "zustand";
import type { CampaignKnobs } from "@/lib/campaign-config";

/**
 * Transient seed ferried from a terminal campaign's Clone action to the
 * composer at `/campaigns/new`. The composer's mount effect calls
 * `consumeSeed()` exactly once and hydrates its local state before first
 * render.
 */
export interface ComposerSeed {
  knobs: CampaignKnobs;
  sourceSet: string[];
  destSet: string[];
}

/**
 * TTL after which a staged seed is considered stale and dropped. If the
 * operator clicks Clone but never lands on `/campaigns/new` (navigation
 * cancelled, browser back, tab switch), the seed would otherwise persist
 * for the rest of the SPA's lifetime and hydrate a future unrelated
 * composer visit.
 */
export const COMPOSER_SEED_TTL_MS = 30_000;

interface StagedSeed {
  value: ComposerSeed;
  stagedAt: number;
}

interface ComposerSeedStore {
  seed: StagedSeed | null;
  /** Stash a seed for the composer's next mount. */
  setSeed: (seed: ComposerSeed) => void;
  /**
   * Read the current seed and clear it in the same tick. Returns `null`
   * when no seed is staged OR when the staged seed has outlived
   * `COMPOSER_SEED_TTL_MS`, so a plain composer mount renders defaults.
   */
  consumeSeed: () => ComposerSeed | null;
}

/**
 * Transient session store — **not** persisted to localStorage. A reload
 * of `/campaigns/new` without a prior Clone click starts from defaults.
 */
export const useComposerSeedStore = create<ComposerSeedStore>((set, get) => ({
  seed: null,
  setSeed: (seed) => set({ seed: { value: seed, stagedAt: Date.now() } }),
  consumeSeed: () => {
    const current = get().seed;
    if (current === null) return null;
    set({ seed: null });
    if (Date.now() - current.stagedAt > COMPOSER_SEED_TTL_MS) return null;
    return current.value;
  },
}));
