import { describe, it, expect } from "vitest";
import {
  reduceOnboarding,
  type OnboardingState,
  type OnboardingEvent,
} from "../electron/first-launch.mjs";

describe("onboarding reducer", () => {
  const initial: OnboardingState = { phase: "pick-vault", vault: null, modelProgress: 0, error: null };

  it("vault-selected → fetching-model", () => {
    const next = reduceOnboarding(initial, {
      type: "vault-selected",
      path: "/home/u/cairn",
    });
    expect(next.phase).toBe("fetching-model");
    expect(next.vault).toBe("/home/u/cairn");
  });

  it("model-progress updates bytes", () => {
    const next = reduceOnboarding(
      { ...initial, phase: "fetching-model", vault: "/x" },
      { type: "model-progress", percent: 42 },
    );
    expect(next.modelProgress).toBe(42);
  });

  it("model-done → ready", () => {
    const next = reduceOnboarding(
      { ...initial, phase: "fetching-model", vault: "/x", modelProgress: 100 },
      { type: "model-done" },
    );
    expect(next.phase).toBe("ready");
  });

  it("error during fetch → error phase, keeps vault", () => {
    const next = reduceOnboarding(
      { ...initial, phase: "fetching-model", vault: "/x" },
      { type: "model-error", message: "disk full" },
    );
    expect(next.phase).toBe("error");
    expect(next.error).toBe("disk full");
    expect(next.vault).toBe("/x");
  });

  it("retry from error → fetching-model", () => {
    const next = reduceOnboarding(
      { ...initial, phase: "error", vault: "/x", error: "disk full" },
      { type: "retry-fetch" },
    );
    expect(next.phase).toBe("fetching-model");
    expect(next.error).toBeNull();
  });

  it("skip-model from error → ready (capability degraded)", () => {
    const next = reduceOnboarding(
      { ...initial, phase: "error", vault: "/x", error: "network" },
      { type: "skip-model" },
    );
    expect(next.phase).toBe("ready");
  });
});
