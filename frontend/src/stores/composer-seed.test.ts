import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { DEFAULT_KNOBS } from "@/lib/campaign-config";
import {
  COMPOSER_SEED_TTL_MS,
  type ComposerSeed,
  useComposerSeedStore,
} from "@/stores/composer-seed";

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

  afterEach(() => {
    vi.useRealTimers();
  });

  test("setSeed + consumeSeed returns the seed and clears the slot", () => {
    const seed = makeSeed();
    useComposerSeedStore.getState().setSeed(seed);
    expect(useComposerSeedStore.getState().seed?.value).toEqual(seed);

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

  test("consumeSeed drops and clears a seed that outlived the TTL", () => {
    // If the operator clicks Clone but never lands on /campaigns/new
    // (cancelled navigation, browser back, tab switch), a stale seed
    // must not hydrate a future unrelated composer visit.
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-21T10:00:00Z"));
    useComposerSeedStore.getState().setSeed(makeSeed());

    vi.setSystemTime(new Date(Date.now() + COMPOSER_SEED_TTL_MS + 1));
    expect(useComposerSeedStore.getState().consumeSeed()).toBeNull();
    expect(useComposerSeedStore.getState().seed).toBeNull();
  });

  test("consumeSeed hydrates a seed within the TTL", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-21T10:00:00Z"));
    const seed = makeSeed();
    useComposerSeedStore.getState().setSeed(seed);

    // Jump forward by just under the TTL — still valid.
    vi.setSystemTime(new Date(Date.now() + COMPOSER_SEED_TTL_MS - 1));
    expect(useComposerSeedStore.getState().consumeSeed()).toEqual(seed);
  });
});
