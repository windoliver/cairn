// First-launch onboarding reducer. The main process owns this; the
// renderer dispatches events via IPC. Pure function so it's easy to
// test in isolation.

/**
 * @typedef {"pick-vault" | "fetching-model" | "ready" | "error"} OnboardingPhase
 * @typedef {{
 *   phase: OnboardingPhase,
 *   vault: string|null,
 *   modelProgress: number,
 *   error: string|null,
 * }} OnboardingState
 *
 * @typedef {
 *   | { type: "vault-selected", path: string }
 *   | { type: "model-progress", percent: number }
 *   | { type: "model-done" }
 *   | { type: "model-error", message: string }
 *   | { type: "retry-fetch" }
 *   | { type: "skip-model" }
 * } OnboardingEvent
 */

/** @returns {OnboardingState} */
export function initialOnboardingState() {
  return { phase: "pick-vault", vault: null, modelProgress: 0, error: null };
}

/**
 * @param {OnboardingState} state
 * @param {OnboardingEvent} event
 * @returns {OnboardingState}
 */
export function reduceOnboarding(state, event) {
  switch (event.type) {
    case "vault-selected":
      return { ...state, phase: "fetching-model", vault: event.path, error: null };
    case "model-progress":
      return { ...state, modelProgress: event.percent };
    case "model-done":
      return { ...state, phase: "ready", modelProgress: 100 };
    case "model-error":
      return { ...state, phase: "error", error: event.message };
    case "retry-fetch":
      return { ...state, phase: "fetching-model", error: null, modelProgress: 0 };
    case "skip-model":
      // Capability is now degraded; user dialog elsewhere informs them
      // that embedding-backed verbs will return CapabilityUnavailable.
      return { ...state, phase: "ready", error: null };
    default:
      return state;
  }
}
