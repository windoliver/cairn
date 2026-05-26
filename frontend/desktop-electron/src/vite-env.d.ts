/// <reference types="vite/client" />

interface Window {
  cairnDesktop?: {
    apiBaseUrl: string;
    /** Per-launch bearer token; null when sidecar auth is disabled. */
    apiToken: string | null;
    /**
     * Subscribe to sidecar address+token changes. The Electron main
     * process emits 'cairn:api-base' after restarting the sidecar on a
     * new ephemeral port; the callback receives the new base URL +
     * fresh token (token may differ on restart).
     */
    onApiBaseChange?: (
      callback: (payload: { url: string; token: string | null }) => void,
    ) => void;
  };
}
