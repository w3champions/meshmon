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

interface ComposerSeedStore {
  seed: ComposerSeed | null;
  /** Stash a seed for the composer's next mount. */
  setSeed: (seed: ComposerSeed) => void;
  /**
   * Read the current seed and clear it in the same tick. Returns `null`
   * when no seed is staged so a plain composer mount renders defaults.
   */
  consumeSeed: () => ComposerSeed | null;
}

/**
 * Transient session store — **not** persisted to localStorage. A reload
 * of `/campaigns/new` without a prior Clone click starts from defaults.
 */
export const useComposerSeedStore = create<ComposerSeedStore>((set, get) => ({
  seed: null,
  setSeed: (seed) => set({ seed }),
  consumeSeed: () => {
    const current = get().seed;
    if (current !== null) set({ seed: null });
    return current;
  },
}));
