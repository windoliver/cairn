import { contextBridge, ipcRenderer } from "electron";

// Main injects the discovered sidecar address into a process.argv
// switch so the renderer learns the URL synchronously without an IPC
// roundtrip per fetch. The bearer TOKEN is deliberately NOT placed on
// argv (visible in `ps`/Activity Monitor to any same-user process);
// renderer pulls it via ipcRenderer.invoke instead.
function argvFlag(name) {
  const prefix = `--${name}=`;
  const found = process.argv.find((a) => a.startsWith(prefix));
  return found ? found.slice(prefix.length) : null;
}

function discoverApiBase() {
  return (
    process.env.CAIRN_DESKTOP_API ||
    argvFlag("cairn-api-base") ||
    "http://127.0.0.1:4000" // last-resort fallback for legacy dev
  );
}

contextBridge.exposeInMainWorld("cairnDesktop", {
  apiBaseUrl: discoverApiBase(),
  /** Fetch the per-launch bearer token from main over IPC. */
  apiToken: () => ipcRenderer.invoke("cairn:token"),
  // Subscribe to address+token changes (e.g. after a sidecar crash +
  // restart on a different port). Callback receives {url, token}.
  onApiBaseChange: (callback) => {
    ipcRenderer.on("cairn:api-base", (_event, payload) => callback(payload));
  },
});
