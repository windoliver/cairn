import { contextBridge, ipcRenderer } from "electron";

// Main injects the discovered sidecar address + bearer token into
// process.argv switches so the renderer learns them synchronously
// without an IPC roundtrip per fetch. CAIRN_DESKTOP_API / TOKEN env
// vars still win for local dev.
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

function discoverApiToken() {
  return (
    process.env.CAIRN_DESKTOP_TOKEN ||
    argvFlag("cairn-api-token") ||
    null
  );
}

contextBridge.exposeInMainWorld("cairnDesktop", {
  apiBaseUrl: discoverApiBase(),
  apiToken: discoverApiToken(),
  // Subscribe to address+token changes (e.g. after a sidecar crash +
  // restart on a different port). Callback receives {url, token}.
  onApiBaseChange: (callback) => {
    ipcRenderer.on("cairn:api-base", (_event, payload) => callback(payload));
  },
});
