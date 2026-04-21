import { beforeEach, describe, expect, test } from "vitest";
import { DEFAULT_KNOBS } from "@/lib/campaign-config";
import { type ComposerSeed, useComposerSeedStore } from "@/stores/composer-seed";

function makeSeed(): ComposerSeed {
  return {
    knobs: { ...DEFAULT_KNOBS, title: "Copy of alpha" },
    sourceSet: ["agent-1", "agent-2"],
    destSet: ["10.0.0.1", "10.0.0.2"],
  };
}

describe("useComposerSeedStore", () => {
  beforeEach(() => {
    // Reset the store between tests — Zustand stores are module-level
    // singletons, so a leftover seed from one test would bleed into the
    // next.
    useComposerSeedStore.setState({ seed: null });
  });

  test("setSeed + consumeSeed returns the seed and clears the slot", () => {
    const seed = makeSeed();
    useComposerSeedStore.getState().setSeed(seed);
    expect(useComposerSeedStore.getState().seed).toEqual(seed);

    const consumed = useComposerSeedStore.getState().consumeSeed();
    expect(consumed).toEqual(seed);
    // Clearing invariant: a second consume returns null so the composer
    // doesn't re-hydrate on a subsequent remount (e.g. HMR).
    expect(useComposerSeedStore.getState().seed).toBeNull();
  });

  test("consumeSeed returns null when no seed has been set", () => {
    expect(useComposerSeedStore.getState().consumeSeed()).toBeNull();
  });

  test("consumeSeed after previous consume returns null", () => {
    useComposerSeedStore.getState().setSeed(makeSeed());
    useComposerSeedStore.getState().consumeSeed();
    expect(useComposerSeedStore.getState().consumeSeed()).toBeNull();
  });
});
